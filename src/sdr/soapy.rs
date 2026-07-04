//! [`SoapySdr`] — libSoapySDR-backed [`Sdr`](super::Sdr) implementation.
//!
//! v0.3.0's single unified SDR backend. Replaces the direct librtlsdr
//! binding (`src/sdr/rtl.rs`) once Phase 2 lands; for now it lives
//! alongside `RtlSdr` so we can validate RTL-SDR parity through the
//! Soapy path before deleting the native code.
//!
//! Devices supported via runtime-loaded Soapy modules:
//!
//! | Driver       | Module DLL              | Status                 |
//! |--------------|-------------------------|------------------------|
//! | `rtlsdr`     | SoapyRTLSDR.dll         | Bench-validated (RTL-SDR Blog V3/V4) |
//! | `sdrplay`    | SoapySDRPlay3.dll       | Bench-validated (RSP1A) |
//! | `airspy`     | SoapyAirspy.dll         | Docs-only, profile ships |
//! | `hackrf`     | SoapyHackRF.dll         | Docs-only, profile ships |
//! | `lime`       | SoapyLMS7.dll           | Docs-only, profile ships |
//! | `plutosdr`   | SoapyPlutoSDR.dll       | Docs-only, profile ships |
//! | `remote`     | SoapyRemote.dll         | Network access to any of the above |
//!
//! Module DLLs are discovered via the `SOAPY_SDR_PLUGIN_PATH` env var,
//! which is set at app startup in portable mode to `bin\SoapySDR\modules0.8`
//! (see `src/main.rs`). In non-portable / dev mode SoapySDR uses its
//! system default.
//!
//! **Sample format strategy.** HD Radio's `nrsc5` pipe expects CU8 IQ
//! (unsigned 8-bit, offset by 128) at 1.488 MS/s. We request `CS8`
//! (signed 8-bit) from Soapy, which most drivers convert natively from
//! whatever bit depth their hardware emits, and add 128 to each byte
//! to produce CU8. RTL-SDR's path is byte-for-byte equivalent to the
//! existing librtlsdr pump (the dongle emits CU8 directly, Soapy's
//! `CS8` is just the CU8 stream with the offset subtracted, which we
//! re-add). SDRplay emits CS16 natively; the SoapySDRPlay3 module
//! converts CS16→CS8 inside the driver — small CPU cost, no quality
//! loss at HD Radio's noise floor.

use num_complex::Complex;
use soapysdr::{Args, Device, Direction, RxStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use super::resampler::IqResampler;
use super::{Sdr, SdrConfig, SdrError, StreamControl};

/// Direction constant — we never transmit.
const RX: Direction = Direction::Rx;
/// Channel index — every supported device exposes channel 0 as the
/// canonical Rx path. Multi-channel devices (e.g. LimeSDR USB) would
/// expose channels 1+ for diversity; HD Radio doesn't use them.
const CH: usize = 0;

/// One open SoapySDR device. Cheap to construct, cheap to clone the
/// `Device` (internally refcounted by libSoapySDR). The `Sdr` trait's
/// `Send + Sync` requirement is satisfied because `soapysdr::Device`
/// is itself `Send + Sync` (libSoapySDR documents all its public C API
/// as thread-safe; the Rust wrapper enforces this).
pub struct SoapySdr {
    /// The wrapped libSoapySDR device handle.
    device: Device,
    /// Driver key (e.g. `"rtlsdr"`, `"sdrplay"`). Cached at open time
    /// so the AGC adapter and UI can route to the right `DeviceProfile`
    /// without re-querying the device.
    driver: String,
    /// Human-readable label for status display ("Realtek, RTL2838UHIDIR,
    /// SN: 00000001"). Cached from the args we opened with.
    // Kept: cached open-time label for status display; not read today.
    #[allow(dead_code)]
    label: String,
    /// Set by `cancel_stream`; read by the `run_stream` loop on each
    /// iteration. Atomic so the control thread can flip it without
    /// blocking on a mutex held by the worker.
    stop_flag: AtomicBool,
    /// Serializes `run_stream` so only one worker can pump the device
    /// at a time. Held for the entire duration of the stream.
    stream_guard: Mutex<()>,
    /// When `Some((src, dst))`, `run_stream` must resample the IQ
    /// stream from `src` sps (what the device is actually producing)
    /// down to `dst` sps (what `nrsc5` expects, normally 1_488_375).
    /// Set in `configure` for drivers whose hardware can't hit
    /// `dst` directly (currently just SDRplay, whose minimum
    /// continuous rate is 2 Msps). `None` means the device is
    /// producing exactly the rate `nrsc5` wants, so the existing
    /// CS8 / CS16 pass-through path is used unchanged.
    resample_rates: Mutex<Option<(f64, f64)>>,
}

/// Lightweight summary of an enumerated device — what the UI device
/// picker turns into a menu entry. The `args` string round-trips back
/// to `SoapySdr::open` to actually open the device.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// Soapy driver key — `rtlsdr`, `sdrplay`, `airspy`, etc.
    pub driver: String,
    /// Human label (`"Realtek RTL2838UHIDIR, SN: 00000001"`).
    pub label: String,
    /// Serial number if the device exposes one — used to disambiguate
    /// multiple dongles of the same model.
    // Kept: parsed device serial for picker disambiguation; not read today.
    #[allow(dead_code)]
    pub serial: Option<String>,
    /// Args string suitable for `SoapySdr::open` to reproduce this device.
    pub device_args: String,
}

impl DeviceInfo {
    /// Return the device-args string with any leading `driver=...,` part
    /// stripped off. Splits the SoapySDR args into the two pieces the
    /// config schema stores separately: `driver` (a single key) and
    /// `device_args` (the per-device disambiguators like
    /// `"serial=00000001"` or `"device=1"`). Used by the SDR Settings
    /// modal when populating `UiCommand::SelectSdrDevice`.
    pub fn args_after_driver(&self) -> String {
        // Soapy's Args::to_string emits comma-separated key=value pairs.
        // We tolerate either order (driver= first or anywhere) by
        // filtering out the matching key wherever it appears.
        let parts: Vec<&str> = self
            .device_args
            .split(',')
            .map(str::trim)
            .filter(|p| !p.starts_with("driver=") && !p.is_empty())
            .collect();
        parts.join(",")
    }
}

impl SoapySdr {
    /// Drivers that NRSC5 Studio currently supports for SDR ingest.
    /// Enumeration may discover other Soapy modules (e.g. audio-only
    /// endpoints), but they are intentionally hidden from the SDR picker.
    const SUPPORTED_DRIVERS: &'static [&'static str] = &[
        "rtlsdr",
        "sdrplay",
        "airspy",
        "hackrf",
        "lime",
        "plutosdr",
        "remote",
    ];

    /// Enumerate every device visible to libSoapySDR.
    ///
    /// **Why we probe per-driver instead of just calling
    /// `enumerate("")` once:** SoapySDR's empty-filter enumerate
    /// asks every loaded module to enumerate, but a single module
    /// that throws during its `findFunction` (e.g. a SoapyRTLSDR
    /// build whose `librtlsdr.dll` can't see `libusb-1.0.dll` until
    /// PATH is correct, or a SoapySDRPlay3 build that doesn't like
    /// the installed SDRplay API version) can cause the whole pass
    /// to bail with zero results. Explicit per-driver passes
    /// isolate each module's failure mode and let the others keep
    /// reporting. Results are merged + deduplicated by `device_args`.
    ///
    /// Returns an empty `Vec` if no devices are present — that is NOT
    /// an error, just "nothing to listen to."
    // Kept: used by examples/soapy_probe.rs (not compiled by a plain
    // `cargo check`); the app calls `enumerate_devices_with_diagnostics`.
    #[allow(dead_code)]
    pub fn enumerate_devices() -> Vec<DeviceInfo> {
        Self::enumerate_devices_with_diagnostics().0
    }

    /// Like [`enumerate_devices`] but also returns a human-readable
    /// diagnostic snapshot. The caller (typically `app.rs`) writes
    /// this snapshot to `<data>\sdr-diagnostics.txt` so a user
    /// reporting "no devices detected" can attach the file without
    /// rebuilding the app.
    pub fn enumerate_devices_with_diagnostics() -> (Vec<DeviceInfo>, String) {
        use std::collections::BTreeMap;

        let mut diag = String::new();
        diag.push_str("# NRSC5 Studio SDR diagnostics\n");
        diag.push_str(&format!(
            "# Captured: {:?}\n\n",
            std::time::SystemTime::now()
        ));
        diag.push_str(&format!(
            "PATH (first 4 entries):\n{}\n\n",
            std::env::var("PATH")
                .unwrap_or_default()
                .split(';')
                .take(4)
                .collect::<Vec<_>>()
                .join("\n")
        ));
        diag.push_str(&format!(
            "SOAPY_SDR_PLUGIN_PATH = {:?}\n",
            std::env::var("SOAPY_SDR_PLUGIN_PATH").ok()
        ));
        diag.push_str(&format!(
            "SOAPY_SDR_ROOT        = {:?}\n\n",
            std::env::var("SOAPY_SDR_ROOT").ok()
        ));

        // Merge per-args (the canonical Soapy args string deduplicates
        // a device that shows up under both an empty-filter pass and
        // a per-driver pass).
        let mut merged: BTreeMap<String, DeviceInfo> = BTreeMap::new();

        // Pass 1: empty filter. Asks every loaded module to enumerate.
        // This works when every module loads cleanly.
        match soapysdr::enumerate("") {
            Ok(v) => {
                diag.push_str(&format!("enumerate(\"\")           -> {} devices\n", v.len()));
                for args in v {
                    let info = args_to_info(args);
                    merged.insert(info.device_args.clone(), info);
                }
            }
            Err(e) => {
                diag.push_str(&format!("enumerate(\"\")           -> ERROR: {e}\n"));
            }
        }

        // Pass 2: per-driver probes. Each is independent so a single
        // module's failure (e.g. SDRplay API not running, libhackrf
        // not on PATH) doesn't suppress the others. The driver list
        // matches the device profiles we ship in `sdr/profile.rs`.
        for driver in Self::SUPPORTED_DRIVERS {
            let filter = format!("driver={driver}");
            match soapysdr::enumerate(&filter[..]) {
                Ok(v) => {
                    diag.push_str(&format!(
                        "enumerate(\"{filter}\") -> {} devices\n",
                        v.len()
                    ));
                    for args in v {
                        let info = args_to_info(args);
                        merged.insert(info.device_args.clone(), info);
                    }
                }
                Err(e) => {
                    diag.push_str(&format!(
                        "enumerate(\"{filter}\") -> ERROR: {e}\n"
                    ));
                }
            }
        }

        let before_filter = merged.len();
        merged.retain(|_, info| {
            Self::SUPPORTED_DRIVERS
                .iter()
                .any(|d| info.driver.eq_ignore_ascii_case(d))
        });

        diag.push_str(&format!("\nTotal unique devices: {}\n", before_filter));
        if before_filter != merged.len() {
            diag.push_str(&format!(
                "Filtered out non-SDR Soapy endpoints: {}\n",
                before_filter - merged.len()
            ));
        }
        diag.push_str(&format!("SDR-visible devices: {}\n", merged.len()));
        for info in merged.values() {
            diag.push_str(&format!("  {} | {}\n", info.driver, info.device_args));
        }

        (merged.into_values().collect(), diag)
    }

    /// Open a device by Soapy args string. `driver=rtlsdr` opens the
    /// first RTL-SDR; `driver=rtlsdr,serial=00000001` picks a specific
    /// one. `driver=remote,remote=192.168.1.50:55132` reaches over the
    /// network via SoapyRemote.
    ///
    /// Does NOT apply any sample-rate / frequency configuration — call
    /// [`Sdr::configure`] next.
    pub fn open(args: &str) -> Result<Self, SdrError> {
        let device = Device::new(args).map_err(|e| SdrError::OpenFailedArgs {
            args: args.to_string(),
            reason: e.to_string(),
        })?;

        // Cache the driver key. libSoapySDR exposes this via the
        // `driver_key` device info field; if it's missing for some
        // reason (shouldn't happen on any mainline module), fall back
        // to whatever's in the args string.
        //
        // **Critical:** the driver-key string returned by Soapy is the
        // module's REGISTERED name, which is mixed-case for several
        // modules (`"SDRplay"`, `"RTLSDR"`, `"HackRF"`, ...). The
        // enumerate-side `args.get("driver")` lookup, by contrast,
        // returns the **lowercase** form because Soapy normalizes
        // keys when parsing args strings. Every per-driver branch
        // in this file (and in [`crate::sdr::profile::lookup`])
        // compares against the lowercase form, so we lowercase the
        // cached value here. Without this normalization, every
        // SDRplay-specific branch -- the rate-snap to 2 Msps, the
        // 1.536 MHz IF-filter override, the single-slider gain UI,
        // the FM/DAB notch defaults -- silently no-ops, which was
        // exactly the bug that kept HD sync from locking on RSP1A.
        let driver = device
            .driver_key()
            .unwrap_or_else(|_| extract_driver_from_args(args).unwrap_or_else(|| "unknown".into()))
            .to_lowercase();

        // Build a human label. Most modules expose hardware key +
        // serial; fall back to driver key alone if those queries
        // fail (some Soapy modules are sloppy about info population).
        let hw_key = device.hardware_key().unwrap_or_default();
        let serial = device
            .hardware_info()
            .ok()
            .and_then(|info| info.get("serial").map(String::from));
        let label = match (hw_key.is_empty(), serial.as_deref()) {
            (false, Some(sn)) => format!("{hw_key} (SN: {sn})"),
            (false, None) => hw_key.clone(),
            (true, Some(sn)) => format!("{driver} (SN: {sn})"),
            (true, None) => driver.clone(),
        };

        Ok(Self {
            device,
            driver,
            label,
            stop_flag: AtomicBool::new(false),
            stream_guard: Mutex::new(()),
            resample_rates: Mutex::new(None),
        })
    }

    /// Soapy driver key for this device (e.g. `"rtlsdr"`). Used by the
    /// AGC adapter and UI to look up the matching `DeviceProfile`.
    pub fn driver(&self) -> &str {
        &self.driver
    }

    /// Human-readable label.
    // Kept: status-display accessor for the cached open-time label;
    // no current caller.
    #[allow(dead_code)]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// List the names of every named gain element this device exposes.
    /// For RTL-SDR this is `["TUNER"]`. For SDRplay it's
    /// `["IFGR", "RFGR"]`. For Airspy it's `["LNA", "MIX", "VGA"]`.
    /// Used by the SDR Settings modal (Phase 3.3) to drive its layout.
    // Kept: gain-element enumeration for the SDR Settings modal, which
    // doesn't consume it yet.
    #[allow(dead_code)]
    pub fn gain_element_names(&self) -> Vec<String> {
        self.device.list_gains(RX, CH).unwrap_or_default()
    }

    /// Get the value of a specific named gain element. Falls back to 0
    /// if the call fails — the SDR Settings modal treats this as a
    /// display-only readout, never blocks on it.
    // Kept: per-element gain readout for the SDR Settings modal;
    // no current caller.
    #[allow(dead_code)]
    pub fn get_gain_element(&self, name: &str) -> f64 {
        self.device.gain_element(RX, CH, name).unwrap_or(0.0)
    }

    /// Set a specific named gain element. AGC adapter uses this via
    /// the `DeviceProfile.agc_element` field; manual gain UI uses it
    /// for whatever the user is dragging.
    pub fn set_gain_element(&self, name: &str, value: f64) -> Result<(), SdrError> {
        self.device
            .set_gain_element(RX, CH, name, value)
            .map_err(|e| SdrError::SoapyCall {
                func: "set_gain_element",
                detail: format!("{name}={value}: {e}"),
            })
    }

    /// Apply PPM frequency correction. SoapySDR exposes this as the
    /// "CORR" frequency component — addressing it via
    /// `set_component_frequency` keeps us off any device-specific
    /// custom-setting path. Zero is a no-op on every driver.
    pub fn set_freq_correction_ppm(&self, ppm: f64) -> Result<(), SdrError> {
        // SoapyRTLSDR, SoapyAirspy, SoapyHackRF all accept the "CORR"
        // frequency component for parts-per-million correction. SDRplay
        // doesn't expose CORR (it has its own internal correction via
        // device args at open time); we silently ignore the error for
        // that path and live with whatever the hardware provides.
        match self
            .device
            .set_component_frequency(RX, CH, "CORR", ppm, "")
        {
            Ok(()) => Ok(()),
            Err(_) if self.driver == "sdrplay" => Ok(()),
            Err(e) => Err(SdrError::SoapyCall {
                func: "set_component_frequency(CORR)",
                detail: format!("ppm={ppm}: {e}"),
            }),
        }
    }

    /// Write a "configure() entered" marker to the diagnostics file
    /// before any fallible Soapy calls happen. The post-configure
    /// dump (see [`write_configure_diagnostics`](Self::write_configure_diagnostics))
    /// only runs on success; if `set_sample_rate` (or any other call)
    /// fails first, this marker is the only signal that `configure`
    /// was even attempted, which makes "Start did nothing" reports
    /// triage-able without a rebuild.
    fn write_configure_marker(&self, cfg: &SdrConfig) {
        let block = format!(
            "\n[configure-enter {driver} @ {ts:?}]\n  \
             requested: rate={req_rate} Hz, freq={req_freq} Hz, ppm={ppm}, gain_tenths={gain:?}\n",
            driver = self.driver,
            ts = std::time::SystemTime::now(),
            req_rate = cfg.sample_rate_sps,
            req_freq = cfg.center_freq_hz,
            ppm = cfg.ppm_correction,
            gain = cfg.initial_gain_tenths,
        );
        if let Some(path) = crate::paths::sdr_diagnostics_file() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| std::io::Write::write_all(&mut f, block.as_bytes()));
        }
    }

    /// Append a post-`configure` state snapshot to the SDR diagnostics
    /// file. Records what we **asked** the driver for vs. what the
    /// driver **reports** after the call, so a "didn't take effect"
    /// silent failure (most commonly `setBandwidth` on older
    /// SoapySDRPlay3 builds) is visible without attaching a debugger.
    ///
    /// All reads are wrapped in `unwrap_or_default()` -- a Soapy
    /// module that doesn't implement a getter shouldn't sink the log
    /// write. The output is appended (not overwritten) so successive
    /// stream starts each leave their own block, which is useful when
    /// debugging "worked once and then stopped" symptoms.
    fn write_configure_diagnostics(&self, cfg: &SdrConfig) {
        // Pull every value back from the device. `unwrap_or` is safe
        // here because a sentinel like -1.0 is recognizable in the
        // log; the alternative ("driver doesn't support this query")
        // would print as 0.0 which is genuinely ambiguous.
        let actual_rate = self.device.sample_rate(RX, CH).unwrap_or(-1.0);
        let actual_freq = self.device.frequency(RX, CH).unwrap_or(-1.0);
        let actual_bw = self.device.bandwidth(RX, CH).unwrap_or(-1.0);
        let actual_gain = self.device.gain(RX, CH).unwrap_or(-1.0);
        let actual_mode = self
            .device
            .gain_mode(RX, CH)
            .map(|b| if b { "AGC" } else { "manual" })
            .unwrap_or("?");

        // SDRplay-specific settings readback. `read_setting` returns
        // a `String` on success; on drivers that don't expose the key
        // we get an `Err`, which we render as "<not exposed>" so the
        // log makes it obvious we asked but the driver said nothing.
        let read_or = |key: &str| -> String {
            self.device
                .read_setting(key)
                .unwrap_or_else(|_| "<not exposed>".to_string())
        };
        let rfgain_sel = read_or("rfgain_sel");
        let rfnotch_ctrl = read_or("rfnotch_ctrl");
        let dabnotch_ctrl = read_or("dabnotch_ctrl");

        // Active antenna readback. Empty string on devices that don't
        // expose antenna selection is the Soapy convention; we render
        // it as `<unnamed>` so the line is never blank.
        let active_antenna = self
            .device
            .antenna(RX, CH)
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "<unnamed>".to_string());

        // Resample plan recap -- mirrors what `run_stream` will do.
        let resample_str = match self.resample_rates.lock().ok().and_then(|g| *g) {
            Some((src, dst)) => format!("{src} -> {dst} Hz (sinc 128-tap)"),
            None => "none (device produces target rate natively)".to_string(),
        };

        let block = format!(
            "\n[configure {driver} @ {ts:?}]\n  \
             requested: rate={req_rate} Hz, freq={req_freq} Hz, ppm={ppm}, gain_tenths={gain:?}, antenna={ant:?}\n  \
             device:    rate={actual_rate} Hz, freq={actual_freq} Hz, bandwidth={actual_bw} Hz\n  \
             gain:      mode={actual_mode}, value={actual_gain} dB\n  \
             antenna:   active={active_antenna}\n  \
             sdrplay:   rfgain_sel={rfgain_sel}, rfnotch_ctrl={rfnotch_ctrl}, dabnotch_ctrl={dabnotch_ctrl}\n  \
             resample:  {resample_str}\n",
            driver = self.driver,
            ts = std::time::SystemTime::now(),
            req_rate = cfg.sample_rate_sps,
            req_freq = cfg.center_freq_hz,
            ppm = cfg.ppm_correction,
            gain = cfg.initial_gain_tenths,
            ant = cfg.antenna,
        );

        if let Some(path) = crate::paths::sdr_diagnostics_file() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            // Append, don't overwrite -- `refresh_sdr_devices` writes
            // the enumeration snapshot to the same file; we want
            // both kinds of data side by side. Failure to open the
            // file (e.g. it's in Notepad with a write lock) just
            // means this snapshot is lost; not worth surfacing.
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| std::io::Write::write_all(&mut f, block.as_bytes()));
        }
    }
}

impl Sdr for SoapySdr {
    fn configure(&self, cfg: &SdrConfig) -> Result<(), SdrError> {
        // Earliest possible diagnostic marker. Recorded *before* any
        // `?` propagation so even a configure that dies on
        // `set_sample_rate` leaves an audit trail in the diagnostics
        // file. Without this it's impossible to tell "configure
        // never ran" from "configure ran but the first call failed".
        self.write_configure_marker(cfg);

        // Sample rate first — some drivers (Airspy) have a discrete
        // set of supported rates and refuse the closest non-native
        // request mid-stream. Setting it pre-tune is safer.
        //
        // **SDRplay rate adjustment.** SDRplay's MSi001/MSi2500 chain
        // can only produce {62.5, 96, 125, 192, 250, 384, 500, 768,
        // 1000} ksps discretely, then a continuous range from
        // 2_000_000 to 10_660_000 sps. nrsc5 needs exactly
        // 1_488_375 sps, which falls in the gap. We ask the device
        // for the lowest supported continuous rate (2 Msps) and rely
        // on the software resampler (see `super::resampler`) to bring
        // it down. Setting `resample_rates = Some((2_000_000,
        // 1_488_375))` here tells `run_stream` to route through the
        // resampler. Every other driver keeps `resample_rates =
        // None` and uses the existing pass-through CS8/CS16 path.
        let (device_rate, resample) =
            if self.driver == "sdrplay" && (cfg.sample_rate_sps as f64) < 2_000_000.0 {
                let target = cfg.sample_rate_sps as f64;
                (2_000_000.0_f64, Some((2_000_000.0_f64, target)))
            } else {
                (cfg.sample_rate_sps as f64, None)
            };
        self.device
            .set_sample_rate(RX, CH, device_rate)
            .map_err(|e| SdrError::SoapyCall {
                func: "set_sample_rate",
                detail: format!("{} Hz: {e}", device_rate as u32),
            })?;
        // Stash the resample plan so `run_stream` can pick it up.
        // Mutex::lock can only fail on poisoning, which would mean a
        // previous thread panicked while holding it -- in which case
        // returning a generic Soapy error here is fine; the upper
        // layer will surface it to the user.
        if let Ok(mut slot) = self.resample_rates.lock() {
            *slot = resample;
        }

        self.device
            .set_frequency(RX, CH, cfg.center_freq_hz as f64, "")
            .map_err(|e| SdrError::SoapyCall {
                func: "set_frequency",
                detail: format!("{} Hz: {e}", cfg.center_freq_hz),
            })?;

        // PPM correction — 0 is a no-op on every driver; non-zero is
        // best-effort (some drivers expose CORR, others don't; see
        // set_freq_correction_ppm).
        if cfg.ppm_correction != 0 {
            self.set_freq_correction_ppm(cfg.ppm_correction as f64)?;
        }

        // Antenna selection. The caller (`start_piped`) resolves the
        // user's persisted choice against the device profile's
        // `default_antenna` and passes the result here. `None` means
        // "leave whatever the driver picked at open time" — fine for
        // single-input devices (RTL-SDR, HackRF, RSP1A). Multi-input
        // devices (RSPduo, RSPdx) get an explicit pick. Best-effort:
        // a driver that doesn't recognize the name falls back to its
        // default; we log via the diagnostics dump rather than
        // erroring out (matches the rfgain_sel / notch_ctrl pattern
        // below).
        //
        // Must run BEFORE the SDRplay-specific writes below because
        // some keys (`rfgain_sel`) and the device's reported gain
        // range are antenna-dependent on RSPdx HiZ.
        if let Some(name) = cfg.antenna.as_deref() {
            if let Err(e) = self.device.set_antenna(RX, CH, name) {
                eprintln!("[soapy] set_antenna({name}) failed: {e}");
            }
        }

        // Direct sampling mode is RTL-SDR-specific. Other drivers
        // either ignore the setting (Airspy) or error on it (SDRplay
        // doesn't have a direct-sampling concept). Apply only on the
        // rtlsdr driver via a Soapy "setting" call.
        if self.driver == "rtlsdr" && cfg.direct_sampling != 0 {
            let mode = match cfg.direct_sampling {
                1 => "1", // I-ADC
                2 => "2", // Q-ADC
                _ => "0",
            };
            // `write_setting` doesn't surface a useful error for
            // unrecognized keys on every Soapy module version; we
            // log via warning instead of erroring out — this is an
            // escape hatch for HF use, not a hot-path setting.
            let _ = self.device.write_setting("direct_samp", mode);
        }

        // SDRplay (RSP1A / RSP1B / RSPduo / RSPdx) specific defaults.
        // Two of these are critical for FM HD radio reception:
        //
        // * `rfgain_sel`: LNA state, integer 0..9 where 0 = MAX
        //   sensitivity (lowest RF gain reduction) and 9 = MIN.
        //   libSoapySDR defaults this to 4 (mid-range), which costs
        //   ~20 dB of front-end gain compared to state 0. The
        //   `set_gain()` call our gain slider drives only adjusts
        //   IFGR (IF gain), not the LNA state -- so without this
        //   write, "max gain" in our UI still leaves the LNA half
        //   asleep and weak HD subcarriers never decode.
        //
        // * `rfnotch_ctrl`: the RSP1A's "RF Notch" is a broadcast-FM
        //   band notch (88-108 MHz), defaulted ON to help users who
        //   tune *outside* FM avoid being desensitized by strong
        //   nearby FM stations. For our use case (tuning *into* FM
        //   for HD radio) it actively filters out the signal we want.
        //
        // * `dabnotch_ctrl`: DAB notch at 174-240 MHz. Outside FM so
        //   leaving it on wouldn't hurt FM HD, but disabling it
        //   keeps the front-end response flat across whatever the
        //   user might tune to next.
        //
        // All three are best-effort writes -- older Soapy module
        // builds may not expose every key, in which case we silently
        // accept the default.
        if self.driver == "sdrplay" {
            let _ = self.device.write_setting("rfgain_sel", "0");
            let _ = self.device.write_setting("rfnotch_ctrl", "false");
            let _ = self.device.write_setting("dabnotch_ctrl", "false");

            // **Analog IF filter bandwidth.** SDRplay's MSi001 has a
            // discrete bandwidth selector: {0.2, 0.3, 0.6, 1.536, 5,
            // 6, 7, 8} MHz. The driver's default is 200 kHz -- fine
            // for narrow-FM voice, but it *filters out the HD Radio
            // digital sidebands*, which sit at ~100-200 kHz from the
            // carrier (total occupied bandwidth ~400 kHz). With the
            // 200-kHz filter active, the analog FM mainlobe is the
            // only thing reaching the ADC and nrsc5 sees a clean
            // analog signal with no recoverable digital content.
            //
            // 1.536 MHz is the right choice for HD Radio at our
            // 1.488 Msps post-resample rate: wide enough to pass
            // both digital sidebands undistorted, narrow enough to
            // suppress adjacent-channel energy that would alias
            // back into our passband. The next step up (5 MHz) is
            // overkill and increases noise integration.
            //
            // Best-effort: a few module revisions of SoapySDRPlay3
            // expose `setBandwidth` as a no-op (filter selection
            // baked into the firmware) -- in which case the call
            // succeeds without changing anything, which is also
            // fine because the firmware default for those builds is
            // already in the right range.
            let _ = self.device.set_bandwidth(RX, CH, 1_536_000.0);

            // **Always disable SDRplay's internal hardware AGC.** The
            // SoapySDRPlay3 module defaults to HW-AGC = on, which
            // overrides any `setGain` call we (or the closed-loop AGC
            // driver thread) make. Symptom of leaving it on: the
            // post-configure diagnostic dump shows `gain: mode=AGC,
            // value=30 dB`, the closed-loop AGC tries to walk to
            // ~40 dB, every tick hammers `setGain` while HW-AGC
            // hammers it back, the USB stream eventually trips a
            // read error and we surface a `lost-device` event. With
            // HW-AGC forced off here, our manual setting (or the
            // closed-loop AGC's tick output) is the only thing
            // touching gain and the stream stays stable.
            //
            // For `Auto` mode this is what the closed-loop AGC needs
            // to function at all. For `Manual` mode the
            // `if let Some(tenths)` block below would have set this
            // anyway; doing it here unconditionally for SDRplay is
            // belt-and-suspenders. For `HardwareAgc` mode -- not
            // supported on SDRplay; the gain-mode UI hides that
            // option for non-RTL drivers -- we'd skip this.
            let _ = self.device.set_gain_mode(RX, CH, false);
        }

        // Initial gain: when `Some`, force manual gain mode and apply.
        // When `None`, leave the device's gain control in whatever
        // mode it was last in (RTL-SDR defaults to hardware AGC on
        // open; SDRplay defaults to manual at mid-range).
        if let Some(tenths) = cfg.initial_gain_tenths {
            // Disable Soapy-level hardware AGC so our manual setting
            // sticks. `set_gain_mode(false)` is the documented way
            // ("automatic" = false means manual).
            self.device
                .set_gain_mode(RX, CH, false)
                .map_err(|e| SdrError::SoapyCall {
                    func: "set_gain_mode",
                    detail: format!("manual: {e}"),
                })?;

            // The Sdr trait's `initial_gain_tenths` is documented as
            // tenths-of-dB on the RTL-SDR TUNER element. For other
            // devices the AGC adapter (Phase 2.3) will translate via
            // the device profile. For Phase 1 parity we set the
            // overall gain directly; the adapter will route per-element
            // later.
            self.device
                .set_gain(RX, CH, tenths as f64 / 10.0)
                .map_err(|e| SdrError::SoapyCall {
                    func: "set_gain",
                    detail: format!("{}: {e}", tenths as f64 / 10.0),
                })?;
        }

        // **Post-configure state dump.** Read back what the driver
        // actually accepted for every setting we tried to apply and
        // append a summary block to the SDR diagnostics file. When
        // a user reports "no HD sync" we (or they) can open the
        // diagnostics file and instantly see whether `set_bandwidth`
        // really moved the IF filter, what sample rate the device is
        // truly running at, where the LNA / IFGR ended up, and which
        // notch filters are engaged. Without this we're guessing
        // from spectrum screenshots.
        //
        // The write is best-effort: failures (no data dir, disk
        // full, file locked by Notepad) do not fail the configure
        // step -- a missing log file is much better than a
        // mysterious "configure failed" toast to the user.
        self.write_configure_diagnostics(cfg);

        Ok(())
    }

    fn gain_table_tenths(&self) -> &[i32] {
        // For Phase 1 RTL-SDR parity we hardcode the R820T table.
        // Phase 2.2 replaces this with a per-driver lookup from
        // `DeviceProfile` (continuous-range devices like SDRplay
        // synthesize a fake table on the fly for AGC's discrete
        // snap-to-nearest behavior).
        super::R820T_GAINS_TENTHS
    }

    fn set_tuner_gain_tenths(&self, tenths: i32) -> Result<(), SdrError> {
        // Phase 1: route to overall gain. Phase 2.3 introduces the
        // adapter that routes to a specific named element via the
        // active `DeviceProfile`.
        self.device
            .set_gain(RX, CH, tenths as f64 / 10.0)
            .map_err(|e| SdrError::SoapyCall {
                func: "set_gain",
                detail: format!("{} tenths: {e}", tenths),
            })
    }

    fn set_center_freq_hz(&self, hz: u32) -> Result<(), SdrError> {
        self.device
            .set_frequency(RX, CH, hz as f64, "")
            .map_err(|e| SdrError::SoapyCall {
                func: "set_frequency",
                detail: format!("{} Hz: {e}", hz),
            })
    }

    fn gain_elements(&self) -> Vec<super::GainElement> {
        // **SDRplay UX collapse.** SoapySDRPlay3 exposes two raw
        // gain elements -- `IFGR` (IF Gain *Reduction*, 20..59 dB)
        // and `RFGR` (RF Gain Reduction / LNA state selector,
        // 0..9 dB). Both knobs have inverted semantics: higher
        // numeric value = MORE reduction = LESS signal gain.
        // Surfacing the raw elements as sliders is misleading
        // (a slider at the right rail looks like "max gain" but
        // actually means "minimum gain") and asks the user to
        // manage two interacting axes by hand.
        //
        // We `configure` already pins the LNA to its most
        // sensitive state (`rfgain_sel=0`) on every connect, and
        // `set_gain()` on libSoapySDRPlay maps directly to IFGR
        // with **un-inverted** semantics (higher dB = more gain,
        // because libSoapySDR internally exposes the aggregate as
        // `MaxIFGR - IFGR`). Synthesizing a single "Gain" element
        // here gives the AGC adapter, manual-gain UI, and config
        // serialization one well-behaved knob to drive.
        //
        // Other drivers keep the existing per-element walk
        // unchanged -- RTL-SDR (TUNER + IF1..IF6) and Airspy
        // (LNA/MIX/VGA) actually benefit from per-element control.
        if self.driver == "sdrplay" {
            // Query the aggregate gain range. libSoapySDRPlay
            // reports this as the device's overall gain (0..48 dB
            // on the RSP1A; ranges vary on RSPduo/RSPdx with
            // antenna selection). Fall back to a conservative
            // hardcoded range if the call fails -- some module
            // builds don't implement the aggregate `getGainRange`.
            let range = self
                .device
                .gain_range(RX, CH)
                .unwrap_or(soapysdr::Range {
                    minimum: 0.0,
                    maximum: 48.0,
                    step: 0.0,
                });
            let current = self.device.gain(RX, CH).unwrap_or(0.0);
            return vec![super::GainElement {
                name: "Gain".to_string(),
                min_db: range.minimum,
                max_db: range.maximum,
                // 1 dB is fine-grained enough that AGC steps look
                // smooth and small enough that the slider feels
                // responsive; the device's internal step may be
                // finer but matters more for analytical tuning
                // than for user-driven HD reception.
                step_db: if range.step > 0.0 { range.step } else { 1.0 },
                current_db: current,
            }];
        }

        // Walk every name reported by `list_gains`, query its range
        // and current value. Skip elements that error on range or
        // value queries (some drivers report element names that don't
        // round-trip through the range query — Airspy's `MIX` element
        // in older Soapy builds is the canonical example).
        let names = match self.device.list_gains(RX, CH) {
            Ok(n) => n,
            Err(_) => return Vec::new(),
        };
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            let range = match self.device.gain_element_range(RX, CH, name.as_str()) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let current = self.device.gain_element(RX, CH, name.as_str()).unwrap_or(0.0);
            out.push(super::GainElement {
                name,
                min_db: range.minimum,
                max_db: range.maximum,
                step_db: range.step,
                current_db: current,
            });
        }
        out
    }

    fn set_gain_element(&self, name: &str, value_db: f64) -> Result<(), SdrError> {
        // SDRplay collapse: the synthetic "Gain" element from
        // `gain_elements()` maps to the device-wide aggregate gain,
        // not a real Soapy element. Route writes to `set_gain()`,
        // which libSoapySDRPlay translates to the correct IFGR
        // value (with un-inverted semantics) while leaving the
        // LNA state we pinned in `configure` alone.
        if self.driver == "sdrplay" && name == "Gain" {
            return self
                .device
                .set_gain(RX, CH, value_db)
                .map_err(|e| SdrError::SoapyCall {
                    func: "set_gain",
                    detail: format!("Gain={value_db}: {e}"),
                });
        }
        // Forward to the concrete inherent method which already
        // wraps SdrError appropriately.
        SoapySdr::set_gain_element(self, name, value_db)
    }

    fn set_frequency_correction_ppm(&self, ppm: f64) -> Result<(), SdrError> {
        SoapySdr::set_freq_correction_ppm(self, ppm)
    }

    fn driver(&self) -> &str {
        SoapySdr::driver(self)
    }

    fn antennas(&self) -> Vec<String> {
        // Soapy returns the antenna list for the current RX channel.
        // Best-effort: drivers without antenna selection report a
        // single unnamed entry, which we filter out below so the UI
        // doesn't show a one-item dropdown. An error from the driver
        // (rare; mostly older HackRF builds) collapses to an empty
        // list, which also hides the dropdown — correct UX since
        // there is nothing useful for the user to pick anyway.
        let raw = self.device.antennas(RX, CH).unwrap_or_default();
        raw.into_iter()
            .filter(|s| !s.is_empty())
            .collect()
    }

    fn antenna(&self) -> Option<String> {
        self.device
            .antenna(RX, CH)
            .ok()
            .filter(|s| !s.is_empty())
    }

    fn set_antenna(&self, name: &str) -> Result<(), SdrError> {
        // The caller (the UI's antenna dropdown) expects this to fully
        // restart the session for a clean re-application of gain
        // setpoints, sample rate, etc. — so we just write the new
        // antenna here without trying to re-clamp in-flight gain. The
        // restart pass through `configure` does the rest.
        self.device
            .set_antenna(RX, CH, name)
            .map_err(|e| SdrError::SoapyCall {
                func: "set_antenna",
                detail: format!("{name}: {e}"),
            })
    }

    fn run_stream(
        &self,
        cb: &mut dyn FnMut(&[u8]) -> StreamControl,
    ) -> Result<(), SdrError> {
        // Only one streamer at a time per device.
        let _guard = self
            .stream_guard
            .try_lock()
            .map_err(|_| SdrError::AlreadyStreaming)?;
        self.stop_flag.store(false, Ordering::Release);

        // Snapshot the resample plan set by `configure`. `Some` means
        // the device produces samples at `src` sps but `nrsc5` wants
        // `dst` sps -- we run a sinc resampler between the driver's
        // RX stream and the user callback. `None` means rates match
        // and we use the legacy pass-through path.
        let resample_plan = self.resample_rates.lock().ok().and_then(|g| *g);

        if let Some((src_rate, dst_rate)) = resample_plan {
            // Build the resampler. If construction fails (numerical
            // edge case from a bad rate), fall back to the pass-
            // through path -- worst case the user hears static
            // instead of a hard error.
            let resampler = IqResampler::new(src_rate, dst_rate)
                .map_err(|e| SdrError::SoapyCall {
                    func: "IqResampler::new",
                    detail: format!("{src_rate} -> {dst_rate}: {e}"),
                })?;
            // SDRplay's SoapySDRPlay3 module advertises both CS16 and
            // CF32 natively, but not CS8. Request CS16 -- f32 would
            // double the USB bandwidth for no quality benefit at the
            // resampler's input.
            let mut rx = self
                .device
                .rx_stream::<Complex<i16>>(&[CH])
                .map_err(|e| SdrError::SoapyCall {
                    func: "rx_stream",
                    detail: format!("CS16 (resample path): {e}"),
                })?;
            return run_resample_loop(&mut rx, &self.stop_flag, resampler, cb);
        }

        // Try CS8 first — byte-for-byte parity with the existing CU8
        // pump after a +128 offset. Fall back to CS16 if the driver
        // doesn't advertise CS8 in its native format list (some Soapy
        // modules expose it via conversion, others don't).
        if let Ok(mut rx) = self.device.rx_stream::<Complex<i8>>(&[CH]) {
            run_cs8_loop(&mut rx, &self.stop_flag, cb)
        } else {
            let mut rx = self
                .device
                .rx_stream::<Complex<i16>>(&[CH])
                .map_err(|e| SdrError::SoapyCall {
                    func: "rx_stream",
                    detail: format!("CS16 fallback: {e}"),
                })?;
            run_cs16_loop(&mut rx, &self.stop_flag, cb)
        }
    }

    fn cancel_stream(&self) -> Result<(), SdrError> {
        self.stop_flag.store(true, Ordering::Release);
        Ok(())
    }

    fn run_stream_cs16(
        &self,
        cb: &mut dyn FnMut(&[i16]) -> StreamControl,
    ) -> Result<(), SdrError> {
        let _guard = self
            .stream_guard
            .try_lock()
            .map_err(|_| SdrError::AlreadyStreaming)?;
        self.stop_flag.store(false, Ordering::Release);

        let resample_plan = self.resample_rates.lock().ok().and_then(|g| *g);

        let mut rx = self
            .device
            .rx_stream::<Complex<i16>>(&[CH])
            .map_err(|e| SdrError::SoapyCall {
                func: "rx_stream",
                detail: format!("CS16 native/resample path: {e}"),
            })?;

        if let Some((src_rate, dst_rate)) = resample_plan {
            let resampler = IqResampler::new(src_rate, dst_rate)
                .map_err(|e| SdrError::SoapyCall {
                    func: "IqResampler::new",
                    detail: format!("{src_rate} -> {dst_rate}: {e}"),
                })?;
            run_resample_cs16_loop(&mut rx, &self.stop_flag, resampler, cb)
        } else {
            run_native_cs16_loop(&mut rx, &self.stop_flag, cb)
        }
    }
}

/// CS8 stream loop. The driver hands us `Complex<i8>` pairs at the
/// configured sample rate; we widen the buffer view to a `&[u8]` (each
/// complex is two bytes — I then Q, packed in declaration order) and
/// add 128 to every byte to convert signed-8 to unsigned-8 CU8 (the
/// format nrsc5's pipe expects).
fn run_cs8_loop(
    rx: &mut RxStream<Complex<i8>>,
    stop_flag: &AtomicBool,
    cb: &mut dyn FnMut(&[u8]) -> StreamControl,
) -> Result<(), SdrError> {
    let mtu = rx.mtu().unwrap_or(16384);
    rx.activate(None).map_err(|e| SdrError::SoapyCall {
        func: "activate",
        detail: e.to_string(),
    })?;

    // Two parallel buffers — one in the i8 format Soapy fills, one in
    // the u8 format we hand to the user callback. Allocate up front,
    // reuse for every transfer.
    let mut i8_buf: Vec<Complex<i8>> = vec![Complex::new(0, 0); mtu];
    let mut u8_buf: Vec<u8> = vec![0u8; mtu * 2];

    let mut error: Option<SdrError> = None;
    let mut overflow_warned = false;
    while !stop_flag.load(Ordering::Acquire) {
        // 1 second timeout — long enough that we won't spin under
        // normal load, short enough that cancel becomes visible
        // quickly even if no samples are flowing (USB stall, etc.).
        let n = match rx.read(&mut [&mut i8_buf], 1_000_000) {
            Ok(n) => n,
            Err(e) => {
                let detail = e.to_string();
                if is_overflow_error(&detail) {
                    if !overflow_warned {
                        eprintln!(
                            "[sdr] transient overflow in CS8 read path; continuing"
                        );
                        overflow_warned = true;
                    }
                    continue;
                }
                error = Some(SdrError::SoapyCall {
                    func: "rx_stream.read",
                    detail,
                });
                break;
            }
        };
        if n == 0 {
            continue;
        }

        // CS8 → CU8: reinterpret each Complex<i8> as two bytes (I, Q),
        // add 128. SAFETY: Complex<i8> is #[repr(C)] (re/im in order),
        // both fields are 1 byte, no padding. We're touching only the
        // first `n` complex samples = `n*2` bytes.
        let src = unsafe {
            std::slice::from_raw_parts(i8_buf.as_ptr() as *const u8, n * 2)
        };
        let dst = &mut u8_buf[..n * 2];
        for (s, d) in src.iter().zip(dst.iter_mut()) {
            *d = (*s).wrapping_add(128);
        }

        if matches!(cb(&dst), StreamControl::Stop) {
            break;
        }
    }

    let _ = rx.deactivate(None);
    if let Some(err) = error {
        // If a cancel landed concurrently with a benign error, prefer
        // the clean-cancel outcome.
        if stop_flag.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(err)
        }
    } else {
        Ok(())
    }
}

/// CS16 fallback loop. Same shape as `run_cs8_loop` but converts
/// `Complex<i16>` → CU8 by dividing each i16 by 256 (≈ >> 8) and
/// adding 128. Used when the driver doesn't expose a CS8 path natively.
fn run_cs16_loop(
    rx: &mut RxStream<Complex<i16>>,
    stop_flag: &AtomicBool,
    cb: &mut dyn FnMut(&[u8]) -> StreamControl,
) -> Result<(), SdrError> {
    let mtu = rx.mtu().unwrap_or(16384);
    rx.activate(None).map_err(|e| SdrError::SoapyCall {
        func: "activate",
        detail: e.to_string(),
    })?;

    let mut i16_buf: Vec<Complex<i16>> = vec![Complex::new(0, 0); mtu];
    let mut u8_buf: Vec<u8> = vec![0u8; mtu * 2];

    let mut error: Option<SdrError> = None;
    let mut overflow_warned = false;
    while !stop_flag.load(Ordering::Acquire) {
        let n = match rx.read(&mut [&mut i16_buf], 1_000_000) {
            Ok(n) => n,
            Err(e) => {
                let detail = e.to_string();
                if is_overflow_error(&detail) {
                    if !overflow_warned {
                        eprintln!(
                            "[sdr] transient overflow in CS16 read path; continuing"
                        );
                        overflow_warned = true;
                    }
                    continue;
                }
                error = Some(SdrError::SoapyCall {
                    func: "rx_stream.read",
                    detail,
                });
                break;
            }
        };
        if n == 0 {
            continue;
        }

        for (i, sample) in i16_buf[..n].iter().enumerate() {
            // CS16 → CU8: right-shift by 8 to land in the i8 range
            // [-128, 127], then XOR the sign bit (0x80) to convert
            // 2's-complement bias to offset-binary bias — exactly the
            // CU8 representation nrsc5 expects. Equivalent to
            // `((sample.re >> 8) + 128) as u8` but a single instruction
            // on every modern CPU.
            u8_buf[i * 2] = ((sample.re >> 8) as u8) ^ 0x80;
            u8_buf[i * 2 + 1] = ((sample.im >> 8) as u8) ^ 0x80;
        }

        if matches!(cb(&u8_buf[..n * 2]), StreamControl::Stop) {
            break;
        }
    }

    let _ = rx.deactivate(None);
    if let Some(err) = error {
        if stop_flag.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(err)
        }
    } else {
        Ok(())
    }
}

fn run_native_cs16_loop(
    rx: &mut RxStream<Complex<i16>>,
    stop_flag: &AtomicBool,
    cb: &mut dyn FnMut(&[i16]) -> StreamControl,
) -> Result<(), SdrError> {
    let mtu = rx.mtu().unwrap_or(16384);
    rx.activate(None).map_err(|e| SdrError::SoapyCall {
        func: "activate",
        detail: e.to_string(),
    })?;

    let mut i16_buf: Vec<Complex<i16>> = vec![Complex::new(0, 0); mtu];

    let mut error: Option<SdrError> = None;
    let mut overflow_warned = false;
    while !stop_flag.load(Ordering::Acquire) {
        let n = match rx.read(&mut [&mut i16_buf], 1_000_000) {
            Ok(n) => n,
            Err(e) => {
                let detail = e.to_string();
                if is_overflow_error(&detail) {
                    if !overflow_warned {
                        eprintln!(
                            "[sdr] transient overflow in native CS16 read path; continuing"
                        );
                        overflow_warned = true;
                    }
                    continue;
                }
                error = Some(SdrError::SoapyCall {
                    func: "rx_stream.read",
                    detail,
                });
                break;
            }
        };
        if n == 0 {
            continue;
        }

        let samples = unsafe {
            std::slice::from_raw_parts(i16_buf.as_ptr() as *const i16, n * 2)
        };
        if matches!(cb(samples), StreamControl::Stop) {
            break;
        }
    }

    let _ = rx.deactivate(None);
    if let Some(err) = error {
        if stop_flag.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(err)
        }
    } else {
        Ok(())
    }
}

/// Resampling stream loop used by devices whose hardware can't hit
/// `nrsc5`'s 1.488375 Msps directly (currently SDRplay only). The
/// driver hands us `Complex<i16>` at whatever native rate
/// [`configure`](SoapySdr::configure) snapped to (e.g. 2 Msps); we
/// convert to `Complex<f32>` normalized to roughly `[-1, 1]`, push
/// through the polyphase sinc resampler in [`super::resampler`], and
/// hand the produced CU8 bytes to `cb` exactly as the pass-through
/// CS8/CS16 loops would.
///
/// The resampler keeps an internal buffer so a partial chunk at the
/// end of one `rx.read` carries over to the next call -- there's no
/// per-block boundary discontinuity in the resampled output.
fn run_resample_loop(
    rx: &mut RxStream<Complex<i16>>,
    stop_flag: &AtomicBool,
    mut resampler: IqResampler,
    cb: &mut dyn FnMut(&[u8]) -> StreamControl,
) -> Result<(), SdrError> {
    let mtu = rx.mtu().unwrap_or(16384);
    rx.activate(None).map_err(|e| SdrError::SoapyCall {
        func: "activate",
        detail: e.to_string(),
    })?;

    // Source buffer for the driver's raw CS16 samples.
    let mut i16_buf: Vec<Complex<i16>> = vec![Complex::new(0, 0); mtu];
    // Per-read scratch buffer of normalized Complex<f32>. Same length
    // as `i16_buf` -- we never push more than one driver-MTU worth at
    // a time into the resampler.
    let mut f32_buf: Vec<Complex<f32>> = vec![Complex::new(0.0, 0.0); mtu];
    // CU8 bytes pulled out of the resampler. Sized generously up
    // front; the resampler appends here and we hand the slice to
    // `cb`, then clear for the next iteration.
    let mut cu8_buf: Vec<u8> = Vec::with_capacity(mtu * 2);

    // CS16 → f32 scale. SDRplay's native CS16 uses the full
    // [-32768, 32767] range; dividing by 32767 puts the result in
    // roughly [-1, 1] -- the input range the resampler's clip-to-CU8
    // conversion expects.
    const I16_NORM: f32 = 1.0 / 32767.0;

    let mut error: Option<SdrError> = None;
    let mut overflow_warned = false;
    while !stop_flag.load(Ordering::Acquire) {
        let n = match rx.read(&mut [&mut i16_buf], 1_000_000) {
            Ok(n) => n,
            Err(e) => {
                let detail = e.to_string();
                if is_overflow_error(&detail) {
                    if !overflow_warned {
                        eprintln!(
                            "[sdr] transient overflow in resample read path; continuing"
                        );
                        overflow_warned = true;
                    }
                    continue;
                }
                error = Some(SdrError::SoapyCall {
                    func: "rx_stream.read",
                    detail,
                });
                break;
            }
        };
        if n == 0 {
            continue;
        }

        // CS16 → f32 conversion. The scalar loop is short enough that
        // the autovectorizer handles it; on x86_64 with AVX2 this
        // measures at ~0.5 ns/sample which is well below the CPU
        // budget at 2 Msps.
        for i in 0..n {
            let s = i16_buf[i];
            f32_buf[i].re = s.re as f32 * I16_NORM;
            f32_buf[i].im = s.im as f32 * I16_NORM;
        }

        // Push into the resampler. `feed` appends ready CU8 bytes to
        // `cu8_buf`; partial chunks stay buffered inside the resampler
        // for the next call.
        cu8_buf.clear();
        resampler.feed(&f32_buf[..n], &mut cu8_buf);

        // Some `rx.read` calls won't produce a full resampler chunk
        // (resampler accumulates internally). In that case `cu8_buf`
        // is empty and we just loop back to grab more samples.
        if cu8_buf.is_empty() {
            continue;
        }

        if matches!(cb(&cu8_buf), StreamControl::Stop) {
            break;
        }
    }

    let _ = rx.deactivate(None);
    if let Some(err) = error {
        if stop_flag.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(err)
        }
    } else {
        Ok(())
    }
}

fn run_resample_cs16_loop(
    rx: &mut RxStream<Complex<i16>>,
    stop_flag: &AtomicBool,
    mut resampler: IqResampler,
    cb: &mut dyn FnMut(&[i16]) -> StreamControl,
) -> Result<(), SdrError> {
    let mtu = rx.mtu().unwrap_or(16384);
    rx.activate(None).map_err(|e| SdrError::SoapyCall {
        func: "activate",
        detail: e.to_string(),
    })?;

    let mut i16_buf: Vec<Complex<i16>> = vec![Complex::new(0, 0); mtu];
    let mut f32_buf: Vec<Complex<f32>> = vec![Complex::new(0.0, 0.0); mtu];
    let mut cs16_buf: Vec<i16> = Vec::with_capacity(mtu * 2);

    const I16_NORM: f32 = 1.0 / 32767.0;

    let mut error: Option<SdrError> = None;
    let mut overflow_warned = false;
    while !stop_flag.load(Ordering::Acquire) {
        let n = match rx.read(&mut [&mut i16_buf], 1_000_000) {
            Ok(n) => n,
            Err(e) => {
                let detail = e.to_string();
                if is_overflow_error(&detail) {
                    if !overflow_warned {
                        eprintln!(
                            "[sdr] transient overflow in CS16 resample read path; continuing"
                        );
                        overflow_warned = true;
                    }
                    continue;
                }
                error = Some(SdrError::SoapyCall {
                    func: "rx_stream.read",
                    detail,
                });
                break;
            }
        };
        if n == 0 {
            continue;
        }

        for i in 0..n {
            let s = i16_buf[i];
            f32_buf[i].re = s.re as f32 * I16_NORM;
            f32_buf[i].im = s.im as f32 * I16_NORM;
        }

        cs16_buf.clear();
        resampler.feed_cs16(&f32_buf[..n], &mut cs16_buf);
        if cs16_buf.is_empty() {
            continue;
        }

        if matches!(cb(&cs16_buf), StreamControl::Stop) {
            break;
        }
    }

    let _ = rx.deactivate(None);
    if let Some(err) = error {
        if stop_flag.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(err)
        }
    } else {
        Ok(())
    }
}

fn is_overflow_error(detail: &str) -> bool {
    detail.to_ascii_lowercase().contains("overflow")
}

/// Translate a Soapy `Args` enumeration record into our `DeviceInfo`.
fn args_to_info(args: Args) -> DeviceInfo {
    let driver = args
        .get("driver")
        .map(|s| s.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let label = args
        .get("label")
        .map(|s| s.to_string())
        .unwrap_or_else(|| driver.clone());
    let serial = args.get("serial").map(|s| s.to_string());

    // Round-trip the args back to a string so the caller can re-open
    // this exact device. Soapy's Args::to_string is the canonical form.
    let device_args = args.to_string();

    DeviceInfo {
        driver,
        label,
        serial,
        device_args,
    }
}

/// Pull the `driver=...` value out of an args string. Used only as a
/// fallback when `Device::driver_key()` fails.
fn extract_driver_from_args(args: &str) -> Option<String> {
    for part in args.split(',') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("driver=") {
            return Some(rest.to_string());
        }
    }
    None
}
