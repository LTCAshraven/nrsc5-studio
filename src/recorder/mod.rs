//! Phase 4: Opus 96 kbps recording.
//!
//! The recorder taps a single decoder's PCM stream off the
//! `SpeakerRouter` (which already drains every registered ring,
//! including the one for the program the user *isn't* currently
//! listening to). That lets us implement the "lock recording to the
//! selected subchannel, independent of the active speaker" model the
//! user asked for in Phase 3 follow-up:
//!
//!   * Listening to HD2 talk show, recording HD1 music    — supported.
//!   * Listening to HD1 music,    recording HD2 talk show — supported.
//!   * Listening to HD1,          recording HD1           — also fine,
//!     the same ring is drained twice (once for the cpal sink, once
//!     for the recorder channel).
//!
//! This module owns the **encoder side**: it consumes 44.1 kHz s16
//! stereo PCM, resamples to 48 kHz (libopus's required input rate is
//! one of 8/12/16/24/48; 44.1 is explicitly *not* supported), splits
//! into 20 ms Opus frames (the standard CELT/Hybrid block size at
//! 48 kHz \u2192 960 samples per channel per frame), feeds them into a
//! `libopus` encoder at 96 kbps VBR, and muxes the resulting packets
//! into an Ogg container on disk.
//!
//! Chunk 4.1 (this commit) only exposes the **single-file** writer
//! surface: open file, push N frames of PCM, close. No segmentation,
//! no metadata, no PSD watching. Chunk 4.2 will wire the
//! `SpeakerRouter` tap into here; Chunk 4.3 adds the GUI + config;
//! Chunk 4.4 layers per-song / continuous splitting and Vorbis
//! metadata tagging on top of this primitive.
//!
//! The encoder runs on a dedicated thread fed by a crossbeam channel
//! of `Vec<i16>` interleaved-stereo chunks. The producer side (the
//! `SpeakerRouter`'s drain loop, attached via Chunk 4.2) only does a
//! cheap `Sender::send` and is wait-free in the common case; all the
//! actual resampling + encoding cost happens on this thread, well
//! out of the audio path.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use crossbeam_channel::{bounded, Receiver, RecvTimeoutError, Sender};
use ogg::writing::{PacketWriteEndInfo, PacketWriter};
use rubato::{Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};

/// Sample rate of the PCM stream coming off `Nrsc5Process`. Matches
/// `crate::audio::NRSC5_SAMPLE_RATE`; redeclared here so the recorder
/// module is self-contained for unit testing.
const INPUT_SAMPLE_RATE: u32 = 44_100;
/// Opus encoder input rate. libopus only accepts 8/12/16/24/48 kHz;
/// 48 kHz keeps every CELT band live and gives us full-bandwidth
/// music quality at 96 kbps.
const OPUS_SAMPLE_RATE: u32 = 48_000;
/// Channel count. nrsc5 always outputs interleaved stereo; mono HD
/// stations get duplicated into both channels upstream of us.
const CHANNELS: usize = 2;
/// Opus frame size in samples per channel. 20 ms \u00d7 48 kHz = 960.
/// This is the standard CELT block; smaller frames (10 ms / 480) give
/// lower latency at the cost of more header overhead, larger frames
/// (40 ms / 1920) give slightly better compression at the cost of
/// recovery latency on packet loss. We never lose packets on local
/// disk so the only reason to pick anything other than 20 ms would
/// be a feature request from the user.
const OPUS_FRAME_SIZE: usize = 960;
/// Target VBR bitrate. 96 kbps stereo is the standard "transparent
/// for most music" sweet spot for Opus and is well above the 64 kbps
/// HD Radio source bitrate \u2014 we're not the limiting factor in audio
/// quality here.
const OPUS_BITRATE_BPS: i32 = 96_000;

/// Bounded channel depth for the PCM pipe from the `SpeakerRouter`
/// tap to the encoder thread. One entry is one drain-tick worth of
/// samples (typ. tens of milliseconds), so 256 entries is several
/// seconds of slack before back-pressure kicks in. The router uses
/// `try_send` so transient encoder-side stalls (e.g. disk flush)
/// just drop the oldest tick instead of stalling the audio path.
const PCM_CHANNEL_DEPTH: usize = 256;

/// Commands the GUI / app layer sends to the recorder thread.
enum RecorderCmd {
    /// Push one tick of interleaved s16 stereo PCM into the encoder.
    /// Sent by the `SpeakerRouter` tap.
    Pcm(Vec<i16>),
    /// Close the current Ogg stream + file and start a new one at
    /// `path` with the given `tags` written into a fresh OpusTags
    /// packet. Used for the max-minutes rotation cap — the encoder
    /// + resampler state is reused across rotations so there's no
    /// audible gap or click at the boundary; only the file changes.
    Rotate { path: PathBuf, tags: RecordingTags },
    /// Flush the current Opus stream and close the file. Sent on
    /// recording stop. The thread exits after handling this command.
    Stop,
}

/// Vorbis-comments payload baked into each file's OpusTags packet.
/// Populated by the app from `StationInfo` + the current frequency
/// at the moment each file (initial or rotation) is opened, so a
/// rotated file always reflects the station the user is *currently*
/// recording (e.g. if they retuned mid-rotation — though Tune
/// currently also stops recording, so in practice the value is
/// stable for the life of one Record/Stop cycle).
#[derive(Debug, Clone, Default)]
pub struct RecordingTags {
    /// Station call sign (e.g. "KEGL-FM"). Empty when SIS hasn't
    /// arrived yet; the tag is omitted entirely in that case.
    pub station: String,
    /// 0-indexed HD subchannel being recorded (i.e. HD<program+1>).
    pub program: u32,
    /// Tuned center frequency in MHz, for the COMMENT field.
    pub frequency_mhz: f32,
    /// Human-readable timestamp of when this *file* started (not the
    /// recording session — each rotation gets a fresh timestamp).
    /// Format: `YYYY-MM-DD HH:MM:SS`.
    pub started_human: String,
    /// ISO 8601 date (`YYYY-MM-DD`) for the DATE Vorbis tag.
    pub date: String,
}

/// Handle to a running recorder thread. Owns the command sender and
/// the join handle so `RecordingSession::stop()` can teardown
/// deterministically.
pub struct RecordingSession {
    cmd_tx: Sender<RecorderCmd>,
    join: Option<JoinHandle<Result<()>>>,
    /// HD subchannel (0..=7) this session is recording. Stored so
    /// the `SpeakerRouter` tap detach can target the right program
    /// on stop without needing to remember it on the app side.
    program: u32,
    /// Path the .opus file is being written to. Surfaced via
    /// `output_path()` so the GUI can show a "saved to ..." toast on
    /// stop.
    output_path: PathBuf,
}

impl RecordingSession {
    /// Spawn a recorder thread writing to `output_path`. Returns the
    /// session handle + a `Sender<Vec<i16>>` the caller (the
    /// `SpeakerRouter` tap) will push PCM samples into.
    ///
    /// `program` is informational \u2014 it gets surfaced on `program()`
    /// and is used when wiring the `SpeakerRouter::AttachRecorder`
    /// command in Chunk 4.2. `tags` is baked into the initial
    /// OpusTags packet; subsequent files (after `rotate`) carry the
    /// tags supplied on the rotate call.
    pub fn spawn(
        program: u32,
        output_path: PathBuf,
        tags: RecordingTags,
    ) -> Result<(Self, Sender<Vec<i16>>)> {
        let (cmd_tx, cmd_rx) = bounded::<RecorderCmd>(PCM_CHANNEL_DEPTH);

        // Pre-create the file outside the worker so the caller gets a
        // synchronous open-error if the directory is missing / read-only,
        // rather than discovering it asynchronously after the GUI has
        // already lit up the "REC" indicator.
        if let Some(parent) = output_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create recording dir {}", parent.display()))?;
            }
        }
        let file = File::create(&output_path)
            .with_context(|| format!("create recording file {}", output_path.display()))?;

        // Encoder / muxer initialization runs on the worker so any
        // failure surfaces via the join-handle's `Result` rather than
        // blocking the GUI on `RecordingSession::spawn`. The error is
        // logged + propagated to the next `stop()` call.
        let output_path_for_thread = output_path.clone();
        let cmd_rx_for_thread = cmd_rx.clone();
        let pcm_tx = SenderProxy::new(cmd_tx.clone());
        let join = std::thread::Builder::new()
            .name(format!("opus-rec-hd{}", program + 1))
            .spawn(move || run_recorder_loop(file, output_path_for_thread, tags, cmd_rx_for_thread))
            .context("spawn opus-rec thread")?;

        Ok((
            Self {
                cmd_tx,
                join: Some(join),
                program,
                output_path,
            },
            pcm_tx.into_pcm_sender(),
        ))
    }

    /// HD subchannel index (0..=7) this session is recording. Lets
    /// the GUI render "REC HD<N>" without remembering the assignment
    /// separately from the session handle.
    pub fn program(&self) -> u32 {
        self.program
    }

    /// Path the .opus file is being written to.
    pub fn output_path(&self) -> &Path {
        &self.output_path
    }

    /// Roll over to a new output file. The current Ogg stream is
    /// flushed + closed (with a valid EOS page), then a fresh file
    /// is opened at `path` and a new Ogg stream is started carrying
    /// `tags`. The encoder + resampler state is kept across the
    /// rotation so there's no audible click at the boundary; only
    /// the file changes. Best-effort \u2014 if the worker is busy the
    /// command is dropped, in which case the next attempt will try
    /// again. Updates `self.output_path` so `output_path()` reflects
    /// the new file.
    pub fn rotate(&mut self, path: PathBuf, tags: RecordingTags) {
        let _ = self.cmd_tx.try_send(RecorderCmd::Rotate {
            path: path.clone(),
            tags,
        });
        self.output_path = path;
    }

    /// Flush + close the recording. Returns the encoder thread's
    /// terminal `Result` so the GUI can surface any deferred I/O or
    /// encoder error. Idempotent in the sense that calling `stop()`
    /// on an already-stopped session returns `Ok(())` immediately.
    pub fn stop(mut self) -> Result<()> {
        // Best-effort \u2014 if the worker has already exited the channel
        // is dropped, which is exactly what we'd want.
        let _ = self.cmd_tx.send(RecorderCmd::Stop);
        if let Some(handle) = self.join.take() {
            match handle.join() {
                Ok(res) => res,
                Err(_) => Err(anyhow!("recorder thread panicked")),
            }
        } else {
            Ok(())
        }
    }
}

impl Drop for RecordingSession {
    fn drop(&mut self) {
        // If the user lets the session handle drop without calling
        // `stop()` (e.g. app exit, panic), do the best-effort
        // flush-and-join so the .opus file ends up with a valid Ogg
        // EOS page instead of being truncated mid-frame.
        let _ = self.cmd_tx.send(RecorderCmd::Stop);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

/// Tiny shim that wraps the `RecorderCmd` sender so the public API
/// only hands out a `Sender<Vec<i16>>` (the `SpeakerRouter` tap
/// doesn't need to know about `RecorderCmd::Stop`). Splitting the
/// channel into "PCM in" + "Stop in" would be cleaner but requires a
/// second channel + a `select!` in the worker; the proxy is one
/// extra allocation per push and we get the typed surface for free.
struct SenderProxy {
    inner: Sender<RecorderCmd>,
}

impl SenderProxy {
    fn new(inner: Sender<RecorderCmd>) -> Self {
        Self { inner }
    }

    /// Convert into a typed PCM sender by wrapping each `Vec<i16>` in
    /// a `RecorderCmd::Pcm`. The router-side push uses `try_send`
    /// (drop-oldest on full) via a per-call closure.
    fn into_pcm_sender(self) -> Sender<Vec<i16>> {
        // We can't directly hand out the inner sender as a
        // `Sender<Vec<i16>>` (different types). Instead, spawn a
        // tiny forwarder thread that translates `Vec<i16>` \u2192
        // `RecorderCmd::Pcm`. Cost is one extra context switch per
        // tick, which at ~5 ms intervals is negligible compared to
        // the Opus encode cost downstream.
        let (tx, rx) = bounded::<Vec<i16>>(PCM_CHANNEL_DEPTH);
        let inner = self.inner;
        std::thread::Builder::new()
            .name("opus-rec-forwarder".to_string())
            .spawn(move || {
                while let Ok(buf) = rx.recv() {
                    // `try_send` not `send`: if the encoder is
                    // backed up (e.g. blocked on disk flush) we'd
                    // rather drop this tick than stall the
                    // `SpeakerRouter`, which also feeds the cpal
                    // sink. The audio path is sacred.
                    let _ = inner.try_send(RecorderCmd::Pcm(buf));
                }
                // Channel closed \u2014 GUI dropped the public sender.
                // Signal stop to the encoder so it flushes cleanly.
                let _ = inner.send(RecorderCmd::Stop);
            })
            .expect("spawn opus-rec-forwarder");
        tx
    }
}

/// Outcome of one inner per-file loop iteration. Drives whether the
/// outer loop opens a fresh file or exits.
enum NextAction {
    /// User-driven Stop or PCM channel went silent — exit cleanly.
    Stop,
    /// Roll over to a new file with new tags; reuse encoder state.
    Rotate { path: PathBuf, tags: RecordingTags },
}

/// Encoder thread body. Owns the resampler, Opus encoder, and Ogg
/// muxer; reads PCM ticks off `cmd_rx`, packs them into 20 ms
/// frames, encodes, and writes packets out. On `Rotate` the current
/// Ogg stream is closed and a fresh one is opened at the new path,
/// while the encoder + resampler state lives on so there's no
/// audible discontinuity at the file boundary.
fn run_recorder_loop(
    initial_file: File,
    initial_output_path: PathBuf,
    initial_tags: RecordingTags,
    cmd_rx: Receiver<RecorderCmd>,
) -> Result<()> {
    // Deterministic Ogg stream serial so multi-stream concatenation
    // works predictably for tooling that cares. A fixed value is
    // fine because we write exactly one Ogg stream per file.
    let serial: u32 = 0x4E52_5343; // "NRSC" \u2014 nrsc5-studio sentinel.

    // ---- Opus encoder (lives across rotations) ----------------------
    let mut encoder = opus::Encoder::new(
        OPUS_SAMPLE_RATE,
        opus::Channels::Stereo,
        opus::Application::Audio,
    )
    .map_err(|e| anyhow!("opus::Encoder::new failed: {e:?}"))?;
    encoder
        .set_bitrate(opus::Bitrate::Bits(OPUS_BITRATE_BPS))
        .map_err(|e| anyhow!("opus set_bitrate failed: {e:?}"))?;
    // VBR is the libopus default but be explicit so a future libopus
    // upgrade that flips the default doesn't silently change our
    // output file size profile.
    encoder
        .set_vbr(true)
        .map_err(|e| anyhow!("opus set_vbr failed: {e:?}"))?;

    // ---- Resampler 44.1k \u2192 48k (lives across rotations) ----------
    // SincFixedIn is the rubato variant that takes a *fixed input
    // chunk size* and produces a variable output chunk size. We feed
    // it `INPUT_CHUNK` samples-per-channel per call; it returns
    // roughly `INPUT_CHUNK * 48000 / 44100` samples per channel on
    // each call. Choosing 441 samples (10 ms) makes the input/output
    // ratio exactly 480/441, so we get 480 output samples per call \u2014
    // half of an Opus 20 ms frame, neat and predictable.
    const INPUT_CHUNK: usize = 441;
    let params = SincInterpolationParameters {
        sinc_len: 128,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 256,
        window: WindowFunction::BlackmanHarris2,
    };
    let mut resampler = SincFixedIn::<f32>::new(
        OPUS_SAMPLE_RATE as f64 / INPUT_SAMPLE_RATE as f64,
        1.0, // no max-rate-ratio fluctuation; we're a fixed-ratio resampler
        params,
        INPUT_CHUNK,
        CHANNELS,
    )
    .map_err(|e| anyhow!("rubato::SincFixedIn::new failed: {e}"))?;

    // ---- State that lives across rotations --------------------------
    // Input scratch: deinterleaved planar f32, one Vec per channel.
    let mut plane_l: Vec<f32> = Vec::with_capacity(INPUT_CHUNK * 4);
    let mut plane_r: Vec<f32> = Vec::with_capacity(INPUT_CHUNK * 4);
    let mut out_plane_l: Vec<f32> = vec![0.0; INPUT_CHUNK * 2];
    let mut out_plane_r: Vec<f32> = vec![0.0; INPUT_CHUNK * 2];
    let mut opus_accum: Vec<i16> = Vec::with_capacity(OPUS_FRAME_SIZE * 2 * 2);
    let mut encoded: Vec<u8> = vec![0u8; 4000];

    // ---- Per-file state (replaced on rotation) ----------------------
    let mut writer = BufWriter::new(initial_file);
    let mut output_path = initial_output_path;
    let mut current_tags = initial_tags;

    // Outer loop: one iteration == one .opus file. We exit on Stop or
    // a tap timeout/disconnect; on Rotate we drop the per-file
    // PacketWriter, swap the underlying file, and loop again.
    loop {
        let mut packet_writer = PacketWriter::new(&mut writer);
        // Cumulative 48 kHz sample count for this file. Granulepos
        // resets on rotation because each file is its own Ogg
        // bitstream.
        let mut granulepos: u64 = 0;
        // Write headers for this file (OpusHead + OpusTags, each on
        // its own page per RFC 7845).
        packet_writer.write_packet(
            build_opus_head(),
            serial,
            PacketWriteEndInfo::EndPage,
            0,
        )?;
        packet_writer.write_packet(
            build_opus_tags(&current_tags),
            serial,
            PacketWriteEndInfo::EndPage,
            0,
        )?;

        // Inner loop: drain Pcm ticks until Stop or Rotate. Returns
        // the action that broke us out so the outer loop knows
        // whether to open a new file or exit.
        let action: NextAction = loop {
            match cmd_rx.recv_timeout(Duration::from_secs(60)) {
                Ok(RecorderCmd::Pcm(samples)) => {
                    push_samples(
                        &samples,
                        &mut plane_l,
                        &mut plane_r,
                        &mut resampler,
                        &mut out_plane_l,
                        &mut out_plane_r,
                        INPUT_CHUNK,
                        &mut opus_accum,
                    );
                    let frame_samples = OPUS_FRAME_SIZE * CHANNELS;
                    while opus_accum.len() >= frame_samples {
                        let n = encoder
                            .encode(&opus_accum[..frame_samples], &mut encoded)
                            .map_err(|e| anyhow!("opus encode failed: {e:?}"))?;
                        granulepos = granulepos.saturating_add(OPUS_FRAME_SIZE as u64);
                        packet_writer.write_packet(
                            encoded[..n].to_vec(),
                            serial,
                            PacketWriteEndInfo::NormalPacket,
                            granulepos,
                        )?;
                        opus_accum.drain(..frame_samples);
                    }
                }
                Ok(RecorderCmd::Rotate { path, tags }) => {
                    break NextAction::Rotate { path, tags };
                }
                Ok(RecorderCmd::Stop) => break NextAction::Stop,
                // No PCM for 60 s \u2014 tap went silent (Stop on the
                // upstream nrsc5 dropped the channel). Flush and
                // exit.
                Err(RecvTimeoutError::Timeout) => break NextAction::Stop,
                Err(RecvTimeoutError::Disconnected) => break NextAction::Stop,
            }
        };

        // Encode + write the final (possibly empty) packet flagged
        // EndStream so the .opus file terminates with a valid Ogg
        // EOS page.
        let frame_samples = OPUS_FRAME_SIZE * CHANNELS;
        if !opus_accum.is_empty() {
            if opus_accum.len() < frame_samples {
                opus_accum.resize(frame_samples, 0);
            }
            let real_samples = opus_accum.len().min(frame_samples);
            let n = encoder
                .encode(&opus_accum[..frame_samples], &mut encoded)
                .map_err(|e| anyhow!("opus final encode failed: {e:?}"))?;
            // granulepos counts only the *real* samples, not the
            // zero padding, so cropping software trims the silence
            // on playback.
            granulepos = granulepos.saturating_add((real_samples / CHANNELS) as u64);
            packet_writer.write_packet(
                encoded[..n].to_vec(),
                serial,
                PacketWriteEndInfo::EndStream,
                granulepos,
            )?;
            opus_accum.clear();
        } else {
            // Empty terminating packet just to flag EOS.
            packet_writer.write_packet(
                Vec::<u8>::new(),
                serial,
                PacketWriteEndInfo::EndStream,
                granulepos,
            )?;
        }

        // Drop the PacketWriter to release the &mut writer borrow
        // before we flush or replace the writer.
        drop(packet_writer);
        writer.flush().with_context(|| {
            format!("flush recording {}", output_path.display())
        })?;

        match action {
            NextAction::Stop => return Ok(()),
            NextAction::Rotate { path, tags } => {
                if let Some(parent) = path.parent() {
                    if !parent.as_os_str().is_empty() {
                        std::fs::create_dir_all(parent).with_context(|| {
                            format!("create recording dir {}", parent.display())
                        })?;
                    }
                }
                let new_file = File::create(&path).with_context(|| {
                    format!("create recording file {}", path.display())
                })?;
                writer = BufWriter::new(new_file);
                output_path = path;
                current_tags = tags;
                // Outer loop restarts — new PacketWriter, granulepos
                // resets, headers re-written for the new bitstream.
            }
        }
    }
}

/// One iteration of the input \u2192 resampler \u2192 frame-accumulator
/// pipeline. Pulls `INPUT_CHUNK` samples-per-channel at a time off
/// the front of `samples`; anything left over is held in the planes
/// (we just append to them and consume in fixed-size strides).
fn push_samples(
    samples: &[i16],
    plane_l: &mut Vec<f32>,
    plane_r: &mut Vec<f32>,
    resampler: &mut SincFixedIn<f32>,
    out_plane_l: &mut Vec<f32>,
    out_plane_r: &mut Vec<f32>,
    input_chunk: usize,
    opus_accum: &mut Vec<i16>,
) {
    // Deinterleave + convert to f32 in [-1.0, 1.0] (rubato's input
    // format). Two channels, interleaved \u2192 two planes.
    for chunk in samples.chunks_exact(2) {
        plane_l.push(chunk[0] as f32 / 32_768.0);
        plane_r.push(chunk[1] as f32 / 32_768.0);
    }
    // Drain as many full INPUT_CHUNK strides as we have.
    while plane_l.len() >= input_chunk {
        let in_slices: [&[f32]; 2] = [&plane_l[..input_chunk], &plane_r[..input_chunk]];
        // rubato's `process_into_buffer` wants `&mut [V: AsMut<[T]>]`,
        // which for our case is `[&mut [f32]; 2]`. Wrap the call in a
        // block so the mut-borrow of out_plane_{l,r} ends before the
        // re-interleave loop reads them by index a few lines down
        // — otherwise the borrow checker (correctly) sees two live
        // mutable references to the same Vec.
        let result = {
            let mut out_slices: [&mut [f32]; 2] = [
                out_plane_l.as_mut_slice(),
                out_plane_r.as_mut_slice(),
            ];
            resampler.process_into_buffer(&in_slices, &mut out_slices, None)
        };
        let (in_used, out_written) = match result {
            Ok(pair) => pair,
            Err(_) => {
                // Resampler error is rare (only happens if the
                // internal buffers can't fit the requested output
                // size, which we sized for). Drop the chunk rather
                // than crash the recorder; the next chunk will
                // resync.
                plane_l.drain(..input_chunk);
                plane_r.drain(..input_chunk);
                continue;
            }
        };
        // Re-interleave + convert back to i16 for the Opus encoder.
        // Opus also accepts f32 input via `encode_float`, but s16 is
        // the canonical HD-Radio sample format and skipping the
        // round-trip is one less place for clipping math to bite.
        for i in 0..out_written {
            let l = (out_plane_l[i].clamp(-1.0, 1.0) * 32_767.0) as i16;
            let r = (out_plane_r[i].clamp(-1.0, 1.0) * 32_767.0) as i16;
            opus_accum.push(l);
            opus_accum.push(r);
        }
        plane_l.drain(..in_used);
        plane_r.drain(..in_used);
    }
}

/// Construct the OpusHead identification packet (RFC 7845 \u00a75.1).
/// Fields:
///   * "OpusHead" (8 bytes)            \u2014 magic
///   * version = 1                     (u8)
///   * channel_count = 2               (u8)
///   * pre_skip = 0                    (u16 LE; we don't apply any
///                                      decoder-side skip, the encoder
///                                      latency is bounded and accepted
///                                      as the leading silence)
///   * input_sample_rate = 48000       (u32 LE; informational only \u2014
///                                      Opus always decodes at 48 kHz
///                                      internally)
///   * output_gain = 0                 (i16 LE; Q7.8 dB)
///   * channel_mapping_family = 0      (u8; 0 = mono/stereo with
///                                      implicit channel order)
fn build_opus_head() -> Vec<u8> {
    let mut h = Vec::with_capacity(19);
    h.extend_from_slice(b"OpusHead");
    h.push(1); // version
    h.push(CHANNELS as u8);
    h.extend_from_slice(&0u16.to_le_bytes()); // pre_skip
    h.extend_from_slice(&OPUS_SAMPLE_RATE.to_le_bytes()); // input rate
    h.extend_from_slice(&0i16.to_le_bytes()); // output gain
    h.push(0); // channel mapping family
    h
}

/// Construct the OpusTags Vorbis-comments packet (RFC 7845 \u00a75.2).
/// Stamps the vendor string plus a small set of station-level
/// comments derived from the supplied `RecordingTags`. PSD timing
/// on real-world stations turns out to be too irregular to reliably
/// drive per-song splitting (lags vary from a few seconds to over a
/// minute), so we don't bother trying to put per-song TITLE /
/// ARTIST tags in here \u2014 only stable, file-lifetime metadata.
/// Format:
///   * "OpusTags" (8 bytes)            \u2014 magic
///   * vendor_string_len (u32 LE) + vendor_string (UTF-8)
///   * user_comment_count (u32 LE)
///   * \u00d7 user_comment_count: len (u32 LE) + bytes (UTF-8)
fn build_opus_tags(tags: &RecordingTags) -> Vec<u8> {
    let vendor = format!("nrsc5-studio {}", env!("CARGO_PKG_VERSION"));
    let vendor_bytes = vendor.as_bytes();

    // Build the user-comment list. Each comment is a "FIELD=value"
    // string per the Vorbis comments spec (RFC 7845 inherits it).
    let mut comments: Vec<String> = Vec::new();
    if !tags.station.is_empty() {
        comments.push(format!("ARTIST={}", tags.station));
        comments.push(format!(
            "ALBUM={} HD{}",
            tags.station,
            tags.program + 1,
        ));
    }
    if !tags.started_human.is_empty() {
        comments.push(format!(
            "TITLE=HD{} recorded {}",
            tags.program + 1,
            tags.started_human,
        ));
    }
    if !tags.date.is_empty() {
        comments.push(format!("DATE={}", tags.date));
    }
    comments.push(format!(
        "COMMENT={:.1} MHz HD{}",
        tags.frequency_mhz,
        tags.program + 1,
    ));

    let mut t = Vec::with_capacity(
        8 + 4 + vendor_bytes.len()
            + 4
            + comments.iter().map(|c| 4 + c.len()).sum::<usize>(),
    );
    t.extend_from_slice(b"OpusTags");
    t.extend_from_slice(&(vendor_bytes.len() as u32).to_le_bytes());
    t.extend_from_slice(vendor_bytes);
    t.extend_from_slice(&(comments.len() as u32).to_le_bytes());
    for c in &comments {
        let bytes = c.as_bytes();
        t.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        t.extend_from_slice(bytes);
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    /// Smoke test: spawn a recorder, push a few seconds of a 440 Hz
    /// stereo tone, stop it, and verify the output file looks like a
    /// valid Ogg Opus stream (begins with "OggS" + the "OpusHead"
    /// identification packet shows up at the expected offset).
    ///
    /// This is the Chunk 4.1 acceptance test \u2014 we're not yet
    /// integrated with `SpeakerRouter`, so this is the only
    /// end-to-end coverage of the encoder/muxer wiring.
    #[test]
    fn writes_valid_ogg_opus_file() {
        let tmp = std::env::temp_dir().join("nrsc5-studio-rec-test.opus");
        let _ = std::fs::remove_file(&tmp);

        let tags = RecordingTags {
            station: "TEST-FM".to_string(),
            program: 0,
            frequency_mhz: 101.1,
            started_human: "2026-05-25 12:00:00".to_string(),
            date: "2026-05-25".to_string(),
        };
        let (session, pcm_tx) =
            RecordingSession::spawn(0, tmp.clone(), tags).expect("spawn");

        // Generate ~1 second of 440 Hz stereo tone at 44.1 kHz s16.
        // Sent as 100 ticks of 10 ms each, mimicking the
        // `SpeakerRouter`'s drain cadence.
        let mut phase: f32 = 0.0;
        let step = 2.0 * std::f32::consts::PI * 440.0 / INPUT_SAMPLE_RATE as f32;
        for _tick in 0..100 {
            let mut buf = Vec::with_capacity(441 * 2);
            for _ in 0..441 {
                let s = (phase.sin() * 16_384.0) as i16;
                buf.push(s);
                buf.push(s);
                phase += step;
            }
            pcm_tx.send(buf).expect("send pcm");
        }
        // Drop the public sender so the forwarder closes the
        // internal channel \u2192 worker receives Stop via the
        // forwarder's drop-handler path.
        drop(pcm_tx);

        session.stop().expect("stop");

        let mut bytes = Vec::new();
        File::open(&tmp)
            .expect("reopen recording")
            .read_to_end(&mut bytes)
            .expect("read recording");
        let _ = std::fs::remove_file(&tmp);

        assert!(bytes.len() > 1000, "recording suspiciously small ({} bytes)", bytes.len());
        assert_eq!(&bytes[..4], b"OggS", "missing Ogg sync bytes");
        // OpusHead lives in the first Ogg page's payload, ~28 bytes
        // in (after the OggS header). Spot-check the magic.
        let head_pos = bytes
            .windows(8)
            .position(|w| w == b"OpusHead")
            .expect("OpusHead magic not found in stream");
        assert!(head_pos < 64, "OpusHead at unexpected offset {head_pos}");
    }
}
