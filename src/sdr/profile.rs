//! Per-driver device profiles.
//!
//! A [`DeviceProfile`] is the static descriptor for one supported SDR
//! family (one Soapy `driver=` key). It encodes everything the rest of
//! the app needs to know about a device without per-driver code paths:
//!
//! * Which gain element the closed-loop AGC controller drives (every
//!   device has a different "right answer" — `TUNER` on RTL-SDR,
//!   `IFGR` on SDRplay, `LNA` on HackRF, etc.).
//! * Whether the AGC value semantically *inverts* the underlying knob.
//!   SDRplay's `IFGR` is gain *reduction* — lower IFGR means more gain,
//!   so the AGC adapter has to flip the sign when translating
//!   `AgcAction { new_tenths: i32 }` into the dB the device expects.
//! * What discrete gain table the AGC controller walks. RTL-SDR's
//!   R820T2 exposes 29 fixed steps via librtlsdr; SDRplay's IFGR is
//!   logically continuous, so we synthesize a 1 dB step table over the
//!   useful range.
//! * Which named elements the SDR Settings modal should render (and in
//!   what order) as manual gain knobs.
//! * Recommended AGC tick rate (RTL-SDR is 500 ms; SDRplay reacts
//!   faster and benefits from 250 ms).
//! * A `bench_validated` flag the SDR Settings modal uses to surface a
//!   "Not bench-validated" banner until a contributor confirms the
//!   profile is correct for their hardware.
//! * Free-form `hd_radio_notes` text shown in the SDR Settings modal —
//!   anything device-specific that affects HD Radio reception
//!   (e.g. SDRplay "use Zero-IF, not Low-IF" caveats).
//!
//! v0.3.0 ships three profiles: `rtlsdr`, `sdrplay`, `hackrf`. The
//! 0.4.0 release will add `airspy`, `lime`, `plutosdr`, and `remote`
//! via the same table; nothing outside this file changes.

/// One row of the device-profile table.
///
/// Static — never modified at runtime. Every field is `&'static` or
/// `Copy` to keep the lookup zero-allocation and the data trivially
/// shareable across threads.
#[derive(Debug, Clone, Copy)]
pub struct DeviceProfile {
    /// Soapy driver key (`"rtlsdr"`, `"sdrplay"`, `"hackrf"`).
    pub driver: &'static str,
    /// Human-readable family name for UI titles. Specific dongles
    /// (RSP1A vs RSPdx, RTL-SDR Blog V3 vs Nooelec) are surfaced by
    /// the device's own `hardware_key` at run time; the profile is
    /// family-level.
    pub display_name: &'static str,
    /// Name of the gain element the closed-loop AGC drives. Must
    /// appear in the device's `list_gains` response — if it doesn't,
    /// the adapter no-ops and logs a warning.
    pub agc_element: &'static str,
    /// dB offset for the AGC mapping. `element_value_db = offset ±
    /// (tenths / 10.0)`, where `±` follows `agc_sign_flip`.
    pub agc_db_offset: f64,
    /// `true` means the AGC's "more amplification" direction maps to
    /// *decreasing* the element value. SDRplay's `IFGR` is gain
    /// reduction (lower = more gain), so this is `true` for that
    /// driver and `false` for RTL-SDR's `TUNER` (which is straight gain).
    pub agc_sign_flip: bool,
    /// Discrete tenths-of-dB table the AGC controller walks. For
    /// devices with continuous gain (SDRplay) this is a synthesized
    /// uniform table; for stepped devices (RTL-SDR) it's the actual
    /// hardware table.
    pub agc_tenths_table: &'static [i32],
    /// Inter-tick interval for the closed-loop AGC driver thread.
    pub agc_tick_ms: u64,
    /// Starting tenths-of-dB the closed-loop AGC applies on its first
    /// tick when in `Auto` mode. Picked per-driver so the controller
    /// starts near the HD-Radio sweet spot for that family instead of
    /// the generic 19.7 dB default (which is fine for RTL-SDR's 0..49
    /// dB table but lands at the bottom of SDRplay's 20..48 dB table
    /// and forces a long climb before MER comes up).
    pub default_agc_initial_tenths: i32,
    /// Names of every gain element the SDR Settings modal should
    /// render as a manual knob, in display order. Always includes the
    /// AGC target. Devices may expose extra elements (e.g. SDRplay's
    /// `RFGR`) that the modal renders alongside.
    pub manual_elements: &'static [&'static str],
    /// Free-form notes shown in the SDR Settings modal under an
    /// "HD Radio notes" disclosure. Cite specific HD Radio-relevant
    /// quirks (Low-IF vs Zero-IF, antenna bias-tee, etc.).
    pub hd_radio_notes: &'static str,
    /// `true` once a contributor has confirmed HD Radio lock + AGC
    /// convergence on real hardware. Used to drive the "Not
    /// bench-validated" banner in the SDR Settings modal.
    pub bench_validated: bool,
}

impl DeviceProfile {
    /// Translate an AGC controller tenths-of-dB value into the actual
    /// dB value to write to the named gain element. Applies the
    /// sign-flip and offset from this profile.
    ///
    /// **Does NOT clamp.** The caller (Phase 2.3's AGC adapter) is
    /// responsible for clamping into the element's reported
    /// `[min_db, max_db]` range — that range is queried per-device at
    /// run time and may be tighter than the synthesized AGC table
    /// suggests (e.g. an early-revision RSPdx that doesn't reach
    /// IFGR=20).
    pub fn agc_tenths_to_element_db(&self, tenths: i32) -> f64 {
        let abs_db = tenths as f64 / 10.0;
        if self.agc_sign_flip {
            self.agc_db_offset - abs_db
        } else {
            self.agc_db_offset + abs_db
        }
    }
}

// === Profile constants ====================================================
//
// One entry per supported driver. Order doesn't matter — lookup is by
// driver key; the SDR picker in Phase 3 sorts these by `display_name`.

/// RTL-SDR (R820T / R820T2 / E4000 / R828D). Native librtlsdr backend
/// is the canonical reference; SoapyRTLSDR wraps it. AGC drives the
/// `TUNER` element in straight-gain dB.
pub const RTLSDR: DeviceProfile = DeviceProfile {
    driver: "rtlsdr",
    display_name: "RTL-SDR (R820T / E4000)",
    agc_element: "TUNER",
    agc_db_offset: 0.0,
    agc_sign_flip: false,
    agc_tenths_table: R820T_GAINS_TENTHS,
    agc_tick_ms: 500,    // R820T2's HD-Radio sweet spot is wide; 19.7 dB is mid-table and
    // matches the long-standing AGC default that produced quick lock
    // on the reference RTL-SDR Blog V3 + dipole bench setup.
    default_agc_initial_tenths: 197,    manual_elements: &["TUNER"],
    hd_radio_notes: "\
        RTL-SDR is the cheapest HD-Radio-capable receiver and the \
        reference platform for this app. Best results: external 12 V \
        bias-tee LNA + outdoor antenna. The R820T2 silently snaps gain \
        to its 29 discrete steps; the app's gain slider matches that \
        same table.",
    bench_validated: true,
};

/// SDRplay (RSP1 / RSP1A / RSP2 / RSPdx / RSPduo). AGC drives the
/// synthetic `"Gain"` element exposed by [`super::soapy::SoapySdr::
/// gain_elements`] for SDRplay devices, which routes to Soapy's
/// `setGain(direction, channel, dB)` overall gain. That call lets
/// the SoapySDRPlay3 module pick the best IFGR+LNA split for the
/// requested aggregate gain, which is what we want for HD Radio --
/// trying to drive IFGR alone (the v0.3.0 behavior) hit two issues:
/// the single-slider UI doesn't expose IFGR as an element so the
/// adapter's `find(e.name == "IFGR")` returned `None` and the AGC
/// silently bailed; and even if it had found it, the LNA was stuck
/// at whatever we set in `configure`, so the AGC couldn't trade LNA
/// for IFGR when the user encountered a strong adjacent.
///
/// The synthesized table walks 20..48 dB of aggregate gain in 1 dB
/// steps. 20 dB is the minimum usable level on a stock RSP1A
/// antenna; 48 dB is the documented `setGain` ceiling (LNA off +
/// IFGR=20). The HD-lock sweet spot is ~40 dB, which is roughly in
/// the middle of the table -- the AGC starts mid-table by default,
/// so first-tick MER is usually high enough to lock without
/// probing. `agc_db_offset=0`, `agc_sign_flip=false` because
/// `setGain` is straight gain (higher dB = more gain), unlike the
/// IFGR semantics we used before.
pub const SDRPLAY: DeviceProfile = DeviceProfile {
    driver: "sdrplay",
    display_name: "SDRplay (RSP1A / duo / dx)",
    agc_element: "Gain",
    agc_db_offset: 0.0,
    agc_sign_flip: false,
    agc_tenths_table: &SDRPLAY_GAIN_TABLE,
    // 500 ms tick instead of 250 ms. SoapySDRPlay3's `setGain` is
    // more disruptive to the USB stream than RTL-SDR's tuner-gain
    // write -- 250 ms ticks were enough to occasionally trip a
    // `lost-device` event when the AGC was probing. 500 ms gives
    // each gain change time to settle into a stable MER reading
    // before the next probe; the steady-state lock time goes up
    // by maybe a second, which is invisible at the UI level.
    agc_tick_ms: 500,
    // Start near the documented HD-Radio sweet spot (~38..44 dB on
    // the RSP1A). Starting at 19.7 dB (the global default) meant the
    // controller spent a noticeable amount of wall time walking up
    // the table before MER came above lock threshold; 38 dB lands
    // right in the middle of the usable HD band so first-tick is
    // usually already inside lock range and the AGC converges fast.
    default_agc_initial_tenths: 380,
    manual_elements: &["Gain"],
    hd_radio_notes: "\
        SDRplay RSPs cover much wider RF range than RTL-SDR and have \
        a real LNA + IF AGC. The app exposes a single aggregate \
        'Gain' slider that ranges 0..48 dB; SoapySDRPlay3 picks the \
        best LNA/IFGR split internally. HD Radio sweet spot on the \
        RSP1A is around 38..44 dB. The FM/DAB notch filters are \
        forced OFF at stream start (they kill the 200 kHz HD \
        sidebands). Requires the SDRplay API service from \
        sdrplay.com (free; can't be bundled).",
    bench_validated: true,
};

/// Synthesized AGC table for SDRplay's aggregate `"Gain"` element --
/// 29 entries from 20.0 dB to 48.0 dB in 1 dB steps, expressed as
/// tenths. Lives at file scope so `agc_tenths_table` can borrow
/// `&'static`.
const SDRPLAY_GAIN_TABLE: [i32; 29] = {
    let mut arr = [0i32; 29];
    let mut i = 0;
    while i < 29 {
        arr[i] = 200 + (i as i32) * 10;
        i += 1;
    }
    arr
};

/// HackRF One. Two-stage analog gain: `LNA` (0..40 dB in 8 dB steps)
/// + `VGA` (0..62 dB in 2 dB steps) + optional `AMP` (a 14 dB front-end
/// boost, on/off only). AGC drives `LNA` since it's the dominant
/// noise-figure-vs-overload tradeoff at HD Radio frequencies.
///
/// Bench validation deferred — no HackRF on hand for the 0.3.0 release.
/// Profile ships so the device picker works; the SDR Settings modal
/// shows a "Not bench-validated" banner until a contributor confirms.
pub const HACKRF: DeviceProfile = DeviceProfile {
    driver: "hackrf",
    display_name: "HackRF One",
    agc_element: "LNA",
    agc_db_offset: 0.0,
    agc_sign_flip: false,
    agc_tenths_table: &HACKRF_LNA_TABLE,
    agc_tick_ms: 500,
    // Mid-table: 24 dB LNA is the documented HackRF starting point
    // for FM HD Radio per the notes below.
    default_agc_initial_tenths: 240,
    manual_elements: &["LNA", "VGA", "AMP"],
    hd_radio_notes: "\
        HackRF One has 8 MHz minimum analog bandwidth which is wide \
        for FM HD Radio's 200 kHz channel — expect strong adjacents \
        to be visible. AMP (the 14 dB front-end boost) usually hurts \
        HD Radio (it pulls broadcast FM out of the ADC headroom). \
        Start with LNA=24, VGA=24, AMP off and adjust from there. \
        Profile is NOT bench-validated; confirm before relying on it.",
    bench_validated: false,
};

/// Synthesized AGC table for HackRF's `LNA` — six 8 dB steps from
/// 0..40 dB, expressed as tenths.
const HACKRF_LNA_TABLE: [i32; 6] = [0, 80, 160, 240, 320, 400];

/// Look up the profile for a given Soapy driver key. Returns `None`
/// for unknown drivers — caller should fall back to a "generic" path
/// (e.g. expose every gain element manually, drive overall gain for
/// AGC) and surface a "Profile not configured for driver `{key}`"
/// warning in the SDR Settings modal.
pub fn lookup(driver: &str) -> Option<&'static DeviceProfile> {
    match driver {
        "rtlsdr" => Some(&RTLSDR),
        "sdrplay" => Some(&SDRPLAY),
        "hackrf" => Some(&HACKRF),
        _ => None,
    }
}

/// All known profiles, in display order. Surfaced to the SDR Settings
/// modal's "Supported devices" section even when no device of that
/// kind is currently plugged in.
pub const ALL_PROFILES: &[&DeviceProfile] = &[&RTLSDR, &SDRPLAY, &HACKRF];

/// R820T2 tuner discrete gain steps in tenths of dB, as reported by
/// librtlsdr's `rtlsdr_get_tuner_gains`. Twenty-nine values covering
/// 0 dB to 49.6 dB. Both the AGC controller (via the RTLSDR profile)
/// and the SDR Settings modal's "snap manual gain to nearest step"
/// logic reference this table. Lives in `profile.rs` (rather than the
/// now-deleted `rtl.rs`) so it survives the legacy backend's removal;
/// SoapyRTLSDR exposes the same steps via its `TUNER` gain element's
/// internal table, so this is still the canonical reference for
/// "what a real RTL-SDR will silently snap to".
pub const R820T_GAINS_TENTHS: &[i32] = &[
    0, 9, 14, 27, 37, 77, 87, 125, 144, 157, 166, 197, 207, 229, 254, 280, 297, 328, 338, 364,
    372, 386, 402, 421, 434, 439, 445, 480, 496,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtlsdr_tenths_round_trip() {
        // Straight gain: agc_tenths_to_element_db(t) = t/10.
        assert_eq!(RTLSDR.agc_tenths_to_element_db(197), 19.7);
        assert_eq!(RTLSDR.agc_tenths_to_element_db(0), 0.0);
        assert_eq!(RTLSDR.agc_tenths_to_element_db(496), 49.6);
    }

    #[test]
    fn sdrplay_aggregate_gain_straight_db() {
        // After the v0.3.1 IFGR→Gain switch, SDRplay uses straight
        // dB (offset=0, no sign flip), same math as RTL-SDR --
        // tenths/10 is the dB value handed to setGain. Asserting
        // the full round-trip keeps a future regression
        // (re-introducing the old offset/sign-flip pair) loud.
        assert_eq!(SDRPLAY.agc_tenths_to_element_db(200), 20.0);
        assert_eq!(SDRPLAY.agc_tenths_to_element_db(400), 40.0);
        assert_eq!(SDRPLAY.agc_tenths_to_element_db(480), 48.0);
    }

    #[test]
    fn sdrplay_synthesized_table_shape() {
        // v0.3.1: table walks 20..48 dB on aggregate `setGain`
        // in 1 dB steps (29 entries). Was 40 entries over IFGR
        // 20..59 in v0.3.0; assertion updated when AGC switched
        // from IFGR to the aggregate Gain element.
        assert_eq!(SDRPLAY.agc_tenths_table.len(), 29);
        assert_eq!(SDRPLAY.agc_tenths_table[0], 200);
        assert_eq!(SDRPLAY.agc_tenths_table[28], 480);
        // Monotonically increasing in 10-tenths (1 dB) steps.
        for win in SDRPLAY.agc_tenths_table.windows(2) {
            assert_eq!(win[1] - win[0], 10);
        }
    }

    #[test]
    fn hackrf_lna_table_matches_hardware_steps() {
        assert_eq!(HACKRF.agc_tenths_table, &[0, 80, 160, 240, 320, 400]);
    }

    #[test]
    fn lookup_known_drivers() {
        assert!(lookup("rtlsdr").is_some());
        assert!(lookup("sdrplay").is_some());
        assert!(lookup("hackrf").is_some());
        assert!(lookup("nonexistent").is_none());
    }

    #[test]
    fn default_initial_tenths_in_each_table() {
        // Every profile's `default_agc_initial_tenths` must be a value
        // the AGC table actually contains (or at least one the
        // controller's `nearest_idx` will snap to without surprise).
        // Asserting membership here is stricter than nearest-snap and
        // catches the common regression of bumping the table without
        // updating the default.
        for prof in ALL_PROFILES {
            assert!(
                prof.agc_tenths_table
                    .contains(&prof.default_agc_initial_tenths),
                "profile {} default_agc_initial_tenths={} not in table",
                prof.driver,
                prof.default_agc_initial_tenths
            );
        }
    }
}
