//! Aggregated SIS (Station Information Service) data for a single tuned
//! HD Radio station.
//!
//! `StationInfo` is the canonical home for everything nrsc5's stderr surfaces
//! about *the station as a whole*, as opposed to the currently-decoded
//! program's PSD (title/artist/album/genre, which still lives in `AppState`).
//! SIS rolls around every few seconds, so fields are `Option<…>` and arrive
//! progressively after sync — the Station Information panel (0.3.5) renders
//! whatever has been observed so far.
//!
//! Lifecycle: `StationInfo::default()` on app start; `reset()` on every
//! retune so a previous station's slogan/location can't bleed into the new
//! one. Brief `LostSync` flickers do *not* reset (that's handled at the
//! AppState layer, not here).
//!
//! Wire format → struct mapping is implemented in `src/ffi/mod.rs` (parser)
//! and `src/app.rs` (event → state). See the per-field comments below for
//! the originating nrsc5 stderr line.

use std::time::Instant;

/// One audio program (subchannel) carried by the station, indexed 0..8 to
/// match HD1..HD8 in the UI. A slot is `Some` once any SIS line has
/// mentioned it; the inner fields fill in independently as SIS repeats.
#[derive(Debug, Clone)]
pub struct ProgramInfo {
    /// Short station name from `SIG Service: type=audio number=N name=…`.
    /// Always present (this is the line that creates the slot).
    pub short_name: String,
    /// Program type from `Audio program N: …, type: <Music|Talk|News|…>`.
    pub program_type: Option<String>,
    /// Sound-experience descriptor from the same line
    /// (`<Mono|Stereo|Binaural|…>`).
    pub sound_experience: Option<String>,
    /// Per-program audio bit rate (kbps), from `Audio bit rate:` lines
    /// observed while this program was selected. Only the currently
    /// decoded program updates this — the others stay `None` until the
    /// user tunes to them.
    pub bit_rate_kbps: Option<f32>,
    /// Wall-clock time the slot was first observed. Used by the panel to
    /// show "seen Xs ago" hints and by `reset_stale` heuristics.
    pub seen_at: Instant,
}

impl ProgramInfo {
    /// Construct a slot from its `SIG Service` short name. All optional
    /// fields are filled in by later events.
    pub fn from_short_name(short_name: String) -> Self {
        Self {
            short_name,
            program_type: None,
            sound_experience: None,
            bit_rate_kbps: None,
            seen_at: Instant::now(),
        }
    }
}

/// One non-audio data service advertised in SIS, e.g. Artist Experience
/// (cover-art channel), traffic, weather, or a station logo channel.
/// From `SIG Service: type=data number=N name=…` plus the optional
/// `Component: …` line that follows.
#[derive(Debug, Clone)]
pub struct DataService {
    /// `number=N` from the SIG Service line. Per-station identifier;
    /// not the same numbering as audio programs.
    pub number: u32,
    /// `name=…` from the SIG Service line.
    pub name: String,
    /// Mime hash as printed by nrsc5 (e.g. `"BE4B7536"`), if a
    /// `Component:` line was observed for this service.
    pub mime: Option<String>,
    /// `service_data_type=N`, if a `Component:` line was observed.
    pub service_data_type: Option<u32>,
}

/// Transmitter geographic location from `Location: <lat>, <lon>, <alt> m`.
#[derive(Debug, Clone, Copy)]
pub struct Location {
    pub latitude: f64,
    pub longitude: f64,
    /// Antenna height above mean sea level, in meters.
    pub altitude_m: i32,
}

/// Exciter or importer equipment metadata (libnrsc5 v3.2.0).
/// Reported once SIS Parameter messages carrying equipment info land.
#[derive(Debug, Clone)]
pub struct EquipmentInfo {
    /// Manufacturer ID string, typically 2 ASCII characters
    /// (e.g. "GG" for Continental, "L7" for Nautel).
    pub manufacturer_id: String,
    /// Core firmware version as a 4-element int array.
    pub core_version: [i32; 4],
    /// 0 = Commercial Release, 1 = Engineering Release, 2 = Patch.
    pub core_status: i32,
    /// Manufacturer-assigned firmware version, 4 elements.
    pub manufacturer_version: [i32; 4],
    /// Same scale as `core_status`.
    pub manufacturer_status: i32,
    /// Whether an importer is wired to this exciter. Only meaningful
    /// for `exciter` slots (always `None` for `importer`).
    pub importer_connected: Option<bool>,
}

impl EquipmentInfo {
    /// Format the core version array as "a.b.c.d".
    pub fn core_version_string(&self) -> String {
        format!(
            "{}.{}.{}.{}",
            self.core_version[0],
            self.core_version[1],
            self.core_version[2],
            self.core_version[3],
        )
    }

    /// Format the manufacturer version array as "a.b.c.d".
    pub fn manufacturer_version_string(&self) -> String {
        format!(
            "{}.{}.{}.{}",
            self.manufacturer_version[0],
            self.manufacturer_version[1],
            self.manufacturer_version[2],
            self.manufacturer_version[3],
        )
    }

    /// Map a `core_status` / `manufacturer_status` value to its label.
    pub fn status_label(status: i32) -> &'static str {
        match status {
            0 => "Commercial",
            1 => "Engineering",
            2 => "Patch",
            _ => "Unknown",
        }
    }
}

/// Broadcaster local-time metadata (libnrsc5 v3.2.0).
#[derive(Debug, Clone, Copy)]
pub struct LocalTimeInfo {
    /// Local Time Zone UTC offset in minutes.
    pub utc_offset_minutes: i32,
    /// DST currently in effect at the broadcaster's region.
    pub dst_regional: bool,
    /// DST practiced locally.
    pub dst_local: bool,
    /// 0 = not practiced, 1 = U.S./Canada schedule, 2 = EU schedule.
    pub dst_schedule: u8,
}

impl LocalTimeInfo {
    /// Format the UTC offset as e.g. "UTC-05:00" or "UTC+09:30".
    pub fn offset_string(&self) -> String {
        let total = self.utc_offset_minutes;
        let sign = if total < 0 { '-' } else { '+' };
        let abs = total.unsigned_abs();
        let h = abs / 60;
        let m = abs % 60;
        format!("UTC{}{:02}:{:02}", sign, h, m)
    }

    /// Short label for `dst_schedule`.
    pub fn dst_schedule_label(&self) -> &'static str {
        match self.dst_schedule {
            0 => "none",
            1 => "US/Canada",
            2 => "EU",
            _ => "unknown",
        }
    }
}

/// GPS leap-second offset broadcast (libnrsc5 v3.2.0).
#[derive(Debug, Clone, Copy)]
pub struct LeapSecondInfo {
    /// Current GPS-UTC offset in seconds (18 as of 2026).
    pub current_offset: i32,
    /// Pending GPS-UTC offset (equals `current_offset` when no
    /// adjustment is scheduled).
    pub pending_offset: i32,
    /// ALFN representing the GPS time of a pending leap second
    /// adjustment, or 0 when no adjustment is pending.
    pub pending_alfn: u32,
}

impl LeapSecondInfo {
    /// True if a leap-second adjustment is scheduled.
    pub fn has_pending(&self) -> bool {
        self.pending_alfn != 0 && self.pending_offset != self.current_offset
    }
}

/// AM-mode SYNC supplementary indicators (libnrsc5 v3.2.0). Stored
/// for diagnostics; not rendered in Phase A.
#[derive(Debug, Clone, Copy)]
pub struct AmSyncIndicators {
    /// Power Level Indicator.
    pub pli: i32,
    /// High-Power PIDS Indicator.
    pub hppi: i32,
    /// Analog Audio Bandwidth Indicator.
    pub aabi: i32,
    /// Reduced Digital Bandwidth Indicator.
    pub rdbi: i32,
}

/// HD Radio service mode (P1/P3/P4 partition layout). nrsc5 does not
/// print this directly in normal stderr output, so the value is *inferred*
/// from the highest audio program number observed in SIG Service lines:
///
/// - Only HD1                 → `Mp1`  (P1 only)
/// - HD1 through HD2–HD3      → `Mp3`  (P1 + P3)
/// - HD4 or HD5 also present  → `Mp11` (P1 + P3 + P4)
///
/// The panel labels this as "inferred" in its tooltip so we're not
/// claiming an authoritative readout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceMode {
    /// MP1 — P1 partition only (HD1 audio).
    Mp1,
    /// MP3 — P1 + P3 partitions (HD1 plus HD2/HD3).
    Mp3,
    /// MP11 — P1 + P3 + P4 partitions (HD1 plus HD2/HD3 plus HD4/HD5).
    Mp11,
}

impl ServiceMode {
    /// Short human-readable label for the panel ("MP1" / "MP3" / "MP11").
    pub fn label(self) -> &'static str {
        match self {
            Self::Mp1 => "MP1",
            Self::Mp3 => "MP3",
            Self::Mp11 => "MP11",
        }
    }
}

/// Aggregated SIS state for the currently tuned station. Lives on
/// `AppState`; populated by the event dispatcher in `src/app.rs` and
/// rendered by the Station Information panel.
#[derive(Debug, Clone, Default)]
pub struct StationInfo {
    /// Call sign from `Station name: …` (e.g. `"KEGL-FM"`). Replaces the
    /// old `AppState::station_name` field.
    pub call_sign: Option<String>,
    /// ISO country code from `Country code: …, FCC facility ID: …`.
    pub country: Option<String>,
    /// FCC facility ID from the same line.
    pub fcc_facility_id: Option<u32>,
    /// Long-form station identifier from `Slogan: …`.
    pub slogan: Option<String>,
    /// Free-text broadcaster message from `Message: …`.
    pub message: Option<String>,
    /// Active emergency-alert text from `Alert: …`. `None` until an alert
    /// is observed; not cleared automatically (alerts persist until the
    /// next retune).
    pub alert: Option<String>,
    /// Transmitter location from `Location: …`.
    pub location: Option<Location>,
    /// HD1..HD8 audio programs, indexed 0..7. `None` for slots the
    /// station hasn't advertised in SIS yet.
    pub programs: [Option<ProgramInfo>; 8],
    /// Non-audio data services advertised in SIS.
    pub data_services: Vec<DataService>,
    /// Exciter equipment metadata (libnrsc5 v3.2.0).
    pub exciter: Option<EquipmentInfo>,
    /// Importer equipment metadata (libnrsc5 v3.2.0).
    pub importer: Option<EquipmentInfo>,
    /// Broadcaster local-time metadata (libnrsc5 v3.2.0).
    pub local_time: Option<LocalTimeInfo>,
    /// GPS leap-second offset broadcast (libnrsc5 v3.2.0).
    pub leap_second: Option<LeapSecondInfo>,
    /// AM-mode SYNC supplementary indicators (libnrsc5 v3.2.0).
    /// Plumbed for diagnostics; not rendered in Phase A.
    pub am_sync: Option<AmSyncIndicators>,
    /// Wall-clock time of the most recent SIS-derived update, used by
    /// the panel's "last updated Xs ago" hint. `None` until the first
    /// field is populated post-retune.
    pub last_updated: Option<Instant>,
}

impl StationInfo {
    /// Wipe all SIS-derived state. Called on every retune so a previous
    /// station's slogan, location, or program list can't leak into the
    /// new one before its first SIS cycle arrives.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// True if any SIS field has been populated since the last reset.
    /// Drives the panel's "Waiting for SIS…" placeholder.
    pub fn has_any_data(&self) -> bool {
        self.last_updated.is_some()
    }

    /// Count of audio program slots that have been observed in SIS.
    /// Used by the program selector to decide how many HD buttons to
    /// light up and by the service-mode heuristic.
    pub fn program_count(&self) -> usize {
        self.programs.iter().filter(|p| p.is_some()).count()
    }

    /// Infer the station's HD Radio service mode (P1/P3/P4 partition
    /// layout) from the highest-numbered program advertised in SIS.
    /// Returns `None` when no programs have been observed yet.
    ///
    /// nrsc5 doesn't print a `Service mode: …` line in normal stderr
    /// output, so this is a heuristic. The panel labels it as inferred
    /// to be honest about that.
    pub fn infer_service_mode(&self) -> Option<ServiceMode> {
        let highest = self
            .programs
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.as_ref().map(|_| i))
            .max()?;
        Some(match highest {
            0 => ServiceMode::Mp1,
            1 | 2 => ServiceMode::Mp3,
            _ => ServiceMode::Mp11,
        })
    }
}
