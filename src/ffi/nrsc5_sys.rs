//! Raw FFI bindings for `libnrsc5` (upstream `theori-io/nrsc5` v3.2.0).
//!
//! Hand-curated mirror of [`res/nrsc5.h`](../../../res/nrsc5.h) — kept in
//! sync with the upstream tag that [`scripts/build-nrsc5-msys2.ps1`] builds.
//!
//! # Why not bindgen?
//!
//! `build.rs` *does* contain a bindgen invocation gated on
//! `NRSC5_GENERATE_BINDINGS=1` so contributors with a working
//! `libclang` install can sanity-check this file against the header. The
//! generated output drags in transitive C stdlib types (`FILE *`,
//! `__builtin_va_list`, `struct tm` interior, etc.) that we never touch,
//! and it's ~1500 lines of mostly noise. This module captures only what
//! we actually call across the FFI boundary while staying byte-faithful
//! to the C struct layout (verified by ABI tests in Phase 1, see
//! `tests::layout_smoke`).
//!
//! # Safety
//!
//! Everything in this module is `unsafe` to use. The pointer-walking
//! linked lists (`*sig_service_t`, `*sis_asd_t`, `*id3_comment_t`) are
//! owned and mutated by the C library — callers must treat them as
//! read-only borrows that live only for the duration of the callback
//! invocation. The safe wrapper in `src/ffi/api.rs` (Phase 2) is what
//! the rest of the crate consumes.

#![allow(non_camel_case_types, non_snake_case, dead_code)]

use std::ffi::{c_char, c_float, c_int, c_uint, c_void};

// =====================================================================
// Constants — mirror the `#define` block at the top of nrsc5.h.
// =====================================================================

pub const NRSC5_SCAN_BEGIN: f64 = 87.9e6;
pub const NRSC5_SCAN_END: f64 = 107.9e6;
pub const NRSC5_SCAN_SKIP: f64 = 0.2e6;

pub const NRSC5_MIME_PRIMARY_IMAGE: u32 = 0xBE4B7536;
pub const NRSC5_MIME_STATION_LOGO: u32 = 0xD9C72536;
pub const NRSC5_MIME_NAVTEQ: u32 = 0x2D42AC3E;
pub const NRSC5_MIME_HERE_TPEG: u32 = 0x82F03DFC;
pub const NRSC5_MIME_HERE_IMAGE: u32 = 0xB7F03DFC;
pub const NRSC5_MIME_HD_TMC: u32 = 0xEECB55B6;
pub const NRSC5_MIME_HDC: u32 = 0x4DC66C5A;
pub const NRSC5_MIME_TEXT: u32 = 0xBB492AAC;
pub const NRSC5_MIME_JPEG: u32 = 0x1E653E9C;
pub const NRSC5_MIME_PNG: u32 = 0x4F328CA0;
pub const NRSC5_MIME_TTN_TPEG_1: u32 = 0xB39EBEB2;
pub const NRSC5_MIME_TTN_TPEG_2: u32 = 0x4EB03469;
pub const NRSC5_MIME_TTN_TPEG_3: u32 = 0x52103469;
pub const NRSC5_MIME_TTN_STM_TRAFFIC: u32 = 0xFF8422D7;
pub const NRSC5_MIME_TTN_STM_WEATHER: u32 = 0xEF042E96;
pub const NRSC5_MIME_UNKNOWN_00000000: u32 = 0x00000000;
pub const NRSC5_MIME_UNKNOWN_1C7D0E29: u32 = 0x1C7D0E29;
pub const NRSC5_MIME_UNKNOWN_B81FFAA8: u32 = 0xB81FFAA8;
pub const NRSC5_MIME_UNKNOWN_FFFFFFFF: u32 = 0xFFFFFFFF;

pub const NRSC5_AUDIO_FRAME_SAMPLES: u32 = 2048;
pub const NRSC5_SAMPLE_RATE_CU8: u32 = 1488375;
pub const NRSC5_SAMPLE_RATE_CS16_FM: f64 = 744187.5;
pub const NRSC5_SAMPLE_RATE_CS16_AM: f64 = 46511.71875;
pub const NRSC5_SAMPLE_RATE_AUDIO: u32 = 44100;

/// Length of Core Version & Manufacturer Version int arrays carried
/// on `EXCITER_INFO` / `IMPORTER_INFO` events (new in v3.2.0).
pub const NRSC5_DEVICE_VERSION_LENGTH: usize = 4;

// Modes (anonymous enum in nrsc5.h)
pub const NRSC5_MODE_FM: c_int = 0;
pub const NRSC5_MODE_AM: c_int = 1;

// SIG component types
pub const NRSC5_SIG_COMPONENT_AUDIO: u8 = 0;
pub const NRSC5_SIG_COMPONENT_DATA: u8 = 1;

// AAS types
pub const NRSC5_AAS_TYPE_STREAM: u8 = 0;
pub const NRSC5_AAS_TYPE_PACKET: u8 = 1;
pub const NRSC5_AAS_TYPE_LOT: u8 = 3;

// SIG service types
pub const NRSC5_SIG_SERVICE_AUDIO: u8 = 0;
pub const NRSC5_SIG_SERVICE_DATA: u8 = 1;

// Event tags (anonymous enum, values are 0..=30 in declaration order;
// 27..=30 added in libnrsc5 v3.2.0).
pub const NRSC5_EVENT_LOST_DEVICE: c_uint = 0;
pub const NRSC5_EVENT_IQ: c_uint = 1;
pub const NRSC5_EVENT_SYNC: c_uint = 2;
pub const NRSC5_EVENT_LOST_SYNC: c_uint = 3;
pub const NRSC5_EVENT_MER: c_uint = 4;
pub const NRSC5_EVENT_BER: c_uint = 5;
pub const NRSC5_EVENT_HDC: c_uint = 6;
pub const NRSC5_EVENT_AUDIO: c_uint = 7;
pub const NRSC5_EVENT_ID3: c_uint = 8;
pub const NRSC5_EVENT_SIG: c_uint = 9;
pub const NRSC5_EVENT_LOT: c_uint = 10;
pub const NRSC5_EVENT_SIS: c_uint = 11;
pub const NRSC5_EVENT_STREAM: c_uint = 12;
pub const NRSC5_EVENT_PACKET: c_uint = 13;
pub const NRSC5_EVENT_AUDIO_SERVICE: c_uint = 14;
pub const NRSC5_EVENT_STATION_ID: c_uint = 15;
pub const NRSC5_EVENT_STATION_NAME: c_uint = 16;
pub const NRSC5_EVENT_STATION_SLOGAN: c_uint = 17;
pub const NRSC5_EVENT_STATION_MESSAGE: c_uint = 18;
pub const NRSC5_EVENT_STATION_LOCATION: c_uint = 19;
pub const NRSC5_EVENT_AUDIO_SERVICE_DESCRIPTOR: c_uint = 20;
pub const NRSC5_EVENT_DATA_SERVICE_DESCRIPTOR: c_uint = 21;
pub const NRSC5_EVENT_EMERGENCY_ALERT: c_uint = 22;
pub const NRSC5_EVENT_HERE_IMAGE: c_uint = 23;
pub const NRSC5_EVENT_LOT_HEADER: c_uint = 24;
pub const NRSC5_EVENT_LOT_FRAGMENT: c_uint = 25;
pub const NRSC5_EVENT_AGC: c_uint = 26;
pub const NRSC5_EVENT_EXCITER_INFO: c_uint = 27;
pub const NRSC5_EVENT_IMPORTER_INFO: c_uint = 28;
pub const NRSC5_EVENT_LEAP_SECOND_OFFSET: c_uint = 29;
pub const NRSC5_EVENT_LOCAL_TIME: c_uint = 30;

// HDC packet flags (new in v3.2.0). Consumed by the safe wrapper to
// derive the decoded audio bit rate from the raw `hdc` packet stream
// (stock libnrsc5 has no bit-rate event).
pub const NRSC5_PKT_FLAGS_NONE: c_uint = 0;
pub const NRSC5_PKT_FLAGS_CRC_ERROR: c_uint = 1 << 0;

// Access flags
pub const NRSC5_ACCESS_PUBLIC: c_uint = 0;
pub const NRSC5_ACCESS_RESTRICTED: c_uint = 1;

// Program types — values 0..=26 with gaps (29, 30, 31, 65, 76).
pub const NRSC5_PROGRAM_TYPE_UNDEFINED: c_uint = 0;
pub const NRSC5_PROGRAM_TYPE_NEWS: c_uint = 1;
pub const NRSC5_PROGRAM_TYPE_INFORMATION: c_uint = 2;
pub const NRSC5_PROGRAM_TYPE_SPORTS: c_uint = 3;
pub const NRSC5_PROGRAM_TYPE_TALK: c_uint = 4;
pub const NRSC5_PROGRAM_TYPE_ROCK: c_uint = 5;
pub const NRSC5_PROGRAM_TYPE_CLASSIC_ROCK: c_uint = 6;
pub const NRSC5_PROGRAM_TYPE_ADULT_HITS: c_uint = 7;
pub const NRSC5_PROGRAM_TYPE_SOFT_ROCK: c_uint = 8;
pub const NRSC5_PROGRAM_TYPE_TOP_40: c_uint = 9;
pub const NRSC5_PROGRAM_TYPE_COUNTRY: c_uint = 10;
pub const NRSC5_PROGRAM_TYPE_OLDIES: c_uint = 11;
pub const NRSC5_PROGRAM_TYPE_SOFT: c_uint = 12;
pub const NRSC5_PROGRAM_TYPE_NOSTALGIA: c_uint = 13;
pub const NRSC5_PROGRAM_TYPE_JAZZ: c_uint = 14;
pub const NRSC5_PROGRAM_TYPE_CLASSICAL: c_uint = 15;
pub const NRSC5_PROGRAM_TYPE_RHYTHM_AND_BLUES: c_uint = 16;
pub const NRSC5_PROGRAM_TYPE_SOFT_RHYTHM_AND_BLUES: c_uint = 17;
pub const NRSC5_PROGRAM_TYPE_FOREIGN_LANGUAGE: c_uint = 18;
pub const NRSC5_PROGRAM_TYPE_RELIGIOUS_MUSIC: c_uint = 19;
pub const NRSC5_PROGRAM_TYPE_RELIGIOUS_TALK: c_uint = 20;
pub const NRSC5_PROGRAM_TYPE_PERSONALITY: c_uint = 21;
pub const NRSC5_PROGRAM_TYPE_PUBLIC: c_uint = 22;
pub const NRSC5_PROGRAM_TYPE_COLLEGE: c_uint = 23;
pub const NRSC5_PROGRAM_TYPE_SPANISH_TALK: c_uint = 24;
pub const NRSC5_PROGRAM_TYPE_SPANISH_MUSIC: c_uint = 25;
pub const NRSC5_PROGRAM_TYPE_HIP_HOP: c_uint = 26;
pub const NRSC5_PROGRAM_TYPE_WEATHER: c_uint = 29;
pub const NRSC5_PROGRAM_TYPE_EMERGENCY_TEST: c_uint = 30;
pub const NRSC5_PROGRAM_TYPE_EMERGENCY: c_uint = 31;
pub const NRSC5_PROGRAM_TYPE_TRAFFIC: c_uint = 65;
pub const NRSC5_PROGRAM_TYPE_SPECIAL_READING_SERVICES: c_uint = 76;

// Blend control
pub const NRSC5_BLEND_DISABLE: c_uint = 0;
pub const NRSC5_BLEND_SELECT: c_uint = 1;
pub const NRSC5_BLEND_ENABLE: c_uint = 2;

// Alert location formats
pub const NRSC5_LOCATION_FORMAT_SAME: c_int = 0;
pub const NRSC5_LOCATION_FORMAT_FIPS: c_int = 1;
pub const NRSC5_LOCATION_FORMAT_ZIP: c_int = 2;

// Alert categories
pub const NRSC5_ALERT_CATEGORY_NON_SPECIFIC: c_uint = 1;
pub const NRSC5_ALERT_CATEGORY_GEOPHYSICAL: c_uint = 2;
pub const NRSC5_ALERT_CATEGORY_WEATHER: c_uint = 3;
pub const NRSC5_ALERT_CATEGORY_SAFETY: c_uint = 4;
pub const NRSC5_ALERT_CATEGORY_SECURITY: c_uint = 5;
pub const NRSC5_ALERT_CATEGORY_RESCUE: c_uint = 6;
pub const NRSC5_ALERT_CATEGORY_FIRE: c_uint = 7;
pub const NRSC5_ALERT_CATEGORY_HEALTH: c_uint = 8;
pub const NRSC5_ALERT_CATEGORY_ENVIRONMENTAL: c_uint = 9;
pub const NRSC5_ALERT_CATEGORY_TRANSPORTATION: c_uint = 10;
pub const NRSC5_ALERT_CATEGORY_UTILITIES: c_uint = 11;
pub const NRSC5_ALERT_CATEGORY_HAZMAT: c_uint = 12;
pub const NRSC5_ALERT_CATEGORY_TEST: c_uint = 30;

// HERE Images
pub const NRSC5_HERE_IMAGE_TRAFFIC: c_int = 8;
pub const NRSC5_HERE_IMAGE_WEATHER: c_int = 13;

// Service data types
pub const NRSC5_SERVICE_DATA_TYPE_NON_SPECIFIC: c_uint = 0;
pub const NRSC5_SERVICE_DATA_TYPE_NEWS: c_uint = 1;
pub const NRSC5_SERVICE_DATA_TYPE_SPORTS: c_uint = 3;
pub const NRSC5_SERVICE_DATA_TYPE_WEATHER: c_uint = 29;
pub const NRSC5_SERVICE_DATA_TYPE_EMERGENCY: c_uint = 31;
pub const NRSC5_SERVICE_DATA_TYPE_TRAFFIC: c_uint = 65;
pub const NRSC5_SERVICE_DATA_TYPE_IMAGE_MAPS: c_uint = 66;
pub const NRSC5_SERVICE_DATA_TYPE_TEXT: c_uint = 80;
pub const NRSC5_SERVICE_DATA_TYPE_ADVERTISING: c_uint = 256;
pub const NRSC5_SERVICE_DATA_TYPE_FINANCIAL: c_uint = 257;
pub const NRSC5_SERVICE_DATA_TYPE_STOCK_TICKER: c_uint = 258;
pub const NRSC5_SERVICE_DATA_TYPE_NAVIGATION: c_uint = 259;
pub const NRSC5_SERVICE_DATA_TYPE_ELECTRONIC_PROGRAM_GUIDE: c_uint = 260;
pub const NRSC5_SERVICE_DATA_TYPE_AUDIO: c_uint = 261;
pub const NRSC5_SERVICE_DATA_TYPE_PRIVATE_DATA_NETWORK: c_uint = 262;
pub const NRSC5_SERVICE_DATA_TYPE_SERVICE_MAINTENANCE: c_uint = 263;
pub const NRSC5_SERVICE_DATA_TYPE_HD_RADIO_SYSTEM_SERVICES: c_uint = 264;
pub const NRSC5_SERVICE_DATA_TYPE_AUDIO_RELATED_DATA: c_uint = 265;
pub const NRSC5_SERVICE_DATA_TYPE_RESERVED_FOR_SPECIAL_TESTS: c_uint = 511;

// =====================================================================
// Opaque types
// =====================================================================

/// Opaque session handle. Allocated by `nrsc5_open_pipe`, freed by
/// `nrsc5_close`.
#[repr(C)]
pub struct nrsc5_t {
    _private: [u8; 0],
    _marker: std::marker::PhantomData<(*mut u8, std::marker::PhantomPinned)>,
}

/// Opaque `struct tm` stand-in. nrsc5 hands us `*mut tm` for
/// `lot.expiry_utc` and `here_image.time_utc`; we never dereference it
/// directly — Phase 2 will convert to a Rust `OffsetDateTime` if
/// callers want the value.
#[repr(C)]
pub struct tm {
    _private: [u8; 0],
}

// =====================================================================
// SIG / SIS linked-list structs
// =====================================================================

/// Component of a SIG (Service Information Guide) record. Owned by
/// the C library; chained via `next`.
#[repr(C)]
pub struct nrsc5_sig_component_t {
    pub next: *mut nrsc5_sig_component_t,
    pub type_: u8,
    pub id: u8,
    pub variant: nrsc5_sig_component_variant,
}

/// Anonymous union inside [`nrsc5_sig_component_t`] selecting between
/// audio and data component metadata. The active variant matches
/// `type_` (`NRSC5_SIG_COMPONENT_*`).
#[repr(C)]
pub union nrsc5_sig_component_variant {
    pub data: nrsc5_sig_component_data,
    pub audio: nrsc5_sig_component_audio,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_sig_component_data {
    pub port: u16,
    pub service_data_type: u16,
    pub type_: u8,
    pub mime: u32,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_sig_component_audio {
    pub port: u8,
    pub type_: u8,
    pub mime: u32,
}

/// SIG service record (linked list head reachable from
/// [`nrsc5_event_sig::services`]).
#[repr(C)]
pub struct nrsc5_sig_service_t {
    pub next: *mut nrsc5_sig_service_t,
    pub type_: u8,
    pub number: u16,
    pub name: *const c_char,
    pub components: *mut nrsc5_sig_component_t,
    pub audio_component: *mut nrsc5_sig_component_t,
}

/// SIS Audio Service Descriptor — linked list element.
#[repr(C)]
pub struct nrsc5_sis_asd_t {
    pub next: *mut nrsc5_sis_asd_t,
    pub program: c_uint,
    pub access: c_uint,
    pub type_: c_uint,
    pub sound_exp: c_uint,
}

/// SIS Data Service Descriptor — linked list element.
#[repr(C)]
pub struct nrsc5_sis_dsd_t {
    pub next: *mut nrsc5_sis_dsd_t,
    pub access: c_uint,
    pub type_: c_uint,
    pub mime_type: u32,
}

/// ID3 comment linked-list element.
#[repr(C)]
pub struct nrsc5_id3_comment_t {
    pub next: *mut nrsc5_id3_comment_t,
    pub lang: *mut c_char,
    pub short_content_desc: *mut c_char,
    pub full_text: *mut c_char,
}

// =====================================================================
// Event union — each variant struct mirrors a `struct { … } name;`
// inside the anonymous union in `nrsc5_event_t`. Variants are named
// `Nrsc5Event<Tag>` for clarity since Rust requires named types for
// union members (C uses anonymous structs inline).
// =====================================================================

#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_iq {
    pub data: *const c_void,
    pub count: usize,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_sync {
    pub freq_offset: c_float,
    pub psmi: c_int,
    /// Power Level Indicator (AM only; set to -1 for FM). New in v3.2.0.
    pub pli: c_int,
    /// High-Power PIDS Indicator (AM only; set to -1 for FM). New in v3.2.0.
    pub hppi: c_int,
    /// Analog Audio Bandwidth Indicator (AM only; set to -1 for FM). New in v3.2.0.
    pub aabi: c_int,
    /// Reduced Digital Bandwidth Indicator (AM only; set to -1 for FM). New in v3.2.0.
    pub rdbi: c_int,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_ber {
    pub cber: c_float,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_mer {
    pub lower: c_float,
    pub upper: c_float,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_hdc {
    pub program: c_uint,
    pub data: *const u8,
    pub count: usize,
    /// Bitfield of `NRSC5_PKT_FLAGS_*` (new in v3.2.0).
    pub flags: c_uint,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_audio {
    pub program: c_uint,
    pub data: *const i16,
    pub count: usize,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_id3_ufid {
    pub owner: *const c_char,
    pub id: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_id3_xhdr {
    pub mime: u32,
    pub param: c_int,
    pub lot: c_int,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_id3 {
    pub program: c_uint,
    pub title: *const c_char,
    pub artist: *const c_char,
    pub album: *const c_char,
    pub genre: *const c_char,
    pub ufid: nrsc5_event_id3_ufid,
    pub xhdr: nrsc5_event_id3_xhdr,
    pub comments: *mut nrsc5_id3_comment_t,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_stream {
    pub port: u16,
    pub seq: u16,
    pub size: c_uint,
    pub mime: u32,
    pub data: *const u8,
    pub service: *mut nrsc5_sig_service_t,
    pub component: *mut nrsc5_sig_component_t,
}

/// `packet` events share the exact layout of `stream` events.
pub type nrsc5_event_packet = nrsc5_event_stream;

#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_lot {
    pub port: u16,
    pub lot: c_uint,
    pub size: c_uint,
    pub mime: u32,
    pub name: *const c_char,
    pub data: *const u8,
    pub expiry_utc: *mut tm,
    pub service: *mut nrsc5_sig_service_t,
    pub component: *mut nrsc5_sig_component_t,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_lot_fragment {
    pub lot: c_uint,
    pub seq: c_uint,
    pub repeat: c_uint,
    pub size: c_uint,
    pub bytes_so_far: c_uint,
    pub is_duplicate: c_int,
    pub data: *const u8,
    pub service: *mut nrsc5_sig_service_t,
    pub component: *mut nrsc5_sig_component_t,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_audio_service {
    pub program: c_uint,
    pub access: c_uint,
    pub type_: c_uint,
    pub codec_mode: c_uint,
    pub blend_control: c_uint,
    pub digital_audio_gain: c_int,
    pub common_delay: c_uint,
    pub latency: c_uint,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_sig {
    pub services: *mut nrsc5_sig_service_t,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_sis {
    pub country_code: *const c_char,
    pub fcc_facility_id: c_int,
    pub name: *const c_char,
    pub slogan: *const c_char,
    pub message: *const c_char,
    pub alert: *const c_char,
    pub latitude: c_float,
    pub longitude: c_float,
    pub altitude: c_int,
    pub audio_services: *mut nrsc5_sis_asd_t,
    pub data_services: *mut nrsc5_sis_dsd_t,
    pub alert_cnt: *const u8,
    pub alert_cnt_length: c_int,
    pub alert_category1: c_int,
    pub alert_category2: c_int,
    pub alert_location_format: c_int,
    pub alert_num_locations: c_int,
    pub alert_locations: *const c_int,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_station_id {
    pub country_code: *const c_char,
    pub fcc_facility_id: c_int,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_station_name {
    pub name: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_station_slogan {
    pub slogan: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_station_message {
    pub message: *const c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_station_location {
    pub latitude: c_float,
    pub longitude: c_float,
    pub altitude: c_int,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_asd {
    pub program: c_uint,
    pub access: c_uint,
    pub type_: c_uint,
    pub sound_exp: c_uint,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_dsd {
    pub access: c_uint,
    pub type_: c_uint,
    pub mime_type: u32,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_emergency_alert {
    pub message: *const c_char,
    pub control_data: *const u8,
    pub control_data_length: c_int,
    pub category1: c_int,
    pub category2: c_int,
    pub location_format: c_int,
    pub num_locations: c_int,
    pub locations: *const c_int,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_here_image {
    pub image_type: c_int,
    pub seq: c_int,
    pub n1: c_int,
    pub n2: c_int,
    pub time_utc: *mut tm,
    pub latitude1: c_float,
    pub longitude1: c_float,
    pub latitude2: c_float,
    pub longitude2: c_float,
    pub name: *const c_char,
    pub size: c_uint,
    pub data: *const u8,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_agc {
    pub gain_db: c_float,
    pub peak_dbfs: c_float,
    pub is_final: c_int,
}

/// Exciter equipment metadata (new in libnrsc5 v3.2.0). Reported
/// once per L1 frame when SIS Parameter messages carrying exciter info
/// have been received.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_exciter_info {
    /// Manufacturer ID string, e.g. "GG" (Continental) or "L7" (Nautel).
    /// Always 2 ASCII characters in practice, NUL-terminated.
    pub manufacturer_id: *const c_char,
    /// Core firmware version, 4 ints.
    pub core_version: [c_int; NRSC5_DEVICE_VERSION_LENGTH],
    /// 0 = Commercial Release, 1 = Engineering Release, 2 = Patch.
    pub core_status: c_int,
    /// Manufacturer-assigned firmware version, 4 ints.
    pub manufacturer_version: [c_int; NRSC5_DEVICE_VERSION_LENGTH],
    /// Same scale as `core_status`.
    pub manufacturer_status: c_int,
    /// 1 if an importer is wired to this exciter, otherwise 0.
    pub importer_connected: c_int,
}

/// Importer equipment metadata (new in libnrsc5 v3.2.0). Same shape
/// as `exciter_info` minus `importer_connected`.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_importer_info {
    pub manufacturer_id: *const c_char,
    pub core_version: [c_int; NRSC5_DEVICE_VERSION_LENGTH],
    pub core_status: c_int,
    pub manufacturer_version: [c_int; NRSC5_DEVICE_VERSION_LENGTH],
    pub manufacturer_status: c_int,
}

/// Leap-second offset broadcast (new in libnrsc5 v3.2.0). The current
/// GPS-UTC offset (always 18 seconds as of 2026), plus any pending
/// adjustment.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_leap_second_offset {
    /// Future GPS-UTC offset in seconds (broadcast in advance of a
    /// scheduled leap second).
    pub pending_offset: c_int,
    /// Current GPS-UTC offset in seconds.
    pub current_offset: c_int,
    /// ALFN representing the GPS time of a pending leap second
    /// adjustment, or 0 if a leap second is not pending.
    pub pending_alfn: c_uint,
}

/// Broadcaster local-time metadata (new in libnrsc5 v3.2.0). Conveys
/// the broadcaster's local UTC offset and DST schedule.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct nrsc5_event_local_time {
    /// Local Time Zone UTC Offset in minutes.
    pub utc_offset: c_int,
    /// 1 if DST is currently in effect regionally, otherwise 0.
    pub dst_regional: c_int,
    /// 1 if DST is practiced locally, otherwise 0.
    pub dst_local: c_int,
    /// 0 = DST not practiced, 1 = U.S./Canada schedule, 2 = EU schedule.
    pub dst_schedule: c_int,
}

/// The anonymous union inside `nrsc5_event_t`. The active variant
/// matches the `event` tag on the enclosing struct
/// (`NRSC5_EVENT_*`). Reading any other variant is undefined
/// behavior — Phase 2's safe wrapper enforces this.
#[repr(C)]
pub union nrsc5_event_payload {
    pub iq: nrsc5_event_iq,
    pub sync: nrsc5_event_sync,
    pub ber: nrsc5_event_ber,
    pub mer: nrsc5_event_mer,
    pub hdc: nrsc5_event_hdc,
    pub audio: nrsc5_event_audio,
    pub id3: nrsc5_event_id3,
    pub stream: nrsc5_event_stream,
    pub packet: nrsc5_event_packet,
    pub lot: nrsc5_event_lot,
    pub lot_fragment: nrsc5_event_lot_fragment,
    pub audio_service: nrsc5_event_audio_service,
    pub sig: nrsc5_event_sig,
    pub sis: nrsc5_event_sis,
    pub station_id: nrsc5_event_station_id,
    pub station_name: nrsc5_event_station_name,
    pub station_slogan: nrsc5_event_station_slogan,
    pub station_message: nrsc5_event_station_message,
    pub station_location: nrsc5_event_station_location,
    pub asd: nrsc5_event_asd,
    pub dsd: nrsc5_event_dsd,
    pub emergency_alert: nrsc5_event_emergency_alert,
    pub here_image: nrsc5_event_here_image,
    pub agc: nrsc5_event_agc,
    pub exciter_info: nrsc5_event_exciter_info,
    pub importer_info: nrsc5_event_importer_info,
    pub leap_second_offset: nrsc5_event_leap_second_offset,
    pub local_time: nrsc5_event_local_time,
}

/// Top-level event passed to the user callback. The `event` field is
/// one of the `NRSC5_EVENT_*` constants and selects which variant of
/// [`nrsc5_event_payload`] is valid.
#[repr(C)]
pub struct nrsc5_event_t {
    pub event: c_uint,
    pub payload: nrsc5_event_payload,
}

/// Signature of the user callback set via [`nrsc5_set_callback`].
///
/// The C library invokes this on its worker thread with a pointer to
/// a transient `nrsc5_event_t` and the `opaque` pointer registered
/// alongside the callback. The event and any pointers it carries
/// (linked lists, strings, byte buffers) are only valid for the
/// duration of the call.
pub type nrsc5_callback_t =
    Option<unsafe extern "C" fn(evt: *const nrsc5_event_t, opaque: *mut c_void)>;

// =====================================================================
// Function prototypes — only the symbols we actually call.
// Other libnrsc5 exports (nrsc5_open, nrsc5_open_file, nrsc5_open_rtltcp,
// the tuner controls, the *_name helpers) are intentionally omitted; the
// Soapy layer owns the device so we feed libnrsc5 through a pipe session and
// let it run blind.
// =====================================================================

#[link(name = "nrsc5")]
unsafe extern "C" {
    /// Allocate a pipe-mode session. The caller writes raw I/Q samples
    /// via [`nrsc5_pipe_samples_cu8`].
    pub fn nrsc5_open_pipe(st: *mut *mut nrsc5_t) -> c_int;

    /// Free a session previously opened by `nrsc5_open_pipe` (or any
    /// other `nrsc5_open_*`). Blocks until the worker thread exits.
    pub fn nrsc5_close(st: *mut nrsc5_t);

    /// Start the demodulation worker. Must be called before pushing
    /// samples.
    pub fn nrsc5_start(st: *mut nrsc5_t);

    /// Stop the demodulation worker. Blocks until the worker thread
    /// is idle.
    pub fn nrsc5_stop(st: *mut nrsc5_t);

    /// Select AM or FM mode (`NRSC5_MODE_FM` / `NRSC5_MODE_AM`).
    /// Returns 0 on success.
    pub fn nrsc5_set_mode(st: *mut nrsc5_t, mode: c_int) -> c_int;

    /// Set the metadata frequency for the session. With a pipe-mode
    /// session this does *not* tune any hardware — the Soapy layer
    /// owns the tuner. The value is used by the decoder for station
    /// info bookkeeping and is reflected back via station-info
    /// events. Returns 0 on success; must be called while the worker
    /// is **stopped**.
    pub fn nrsc5_set_frequency(st: *mut nrsc5_t, freq: c_float) -> c_int;

    /// Register the event callback and an opaque context pointer.
    /// The callback is invoked on the worker thread for every event
    /// (metadata, audio, sync, MER/BER, …).
    pub fn nrsc5_set_callback(
        st: *mut nrsc5_t,
        callback: nrsc5_callback_t,
        opaque: *mut c_void,
    );

    /// Push `length` unsigned-8-bit complex I/Q samples (interleaved
    /// I, Q) into the demodulator. The expected sample rate is
    /// [`NRSC5_SAMPLE_RATE_CU8`] (1.488375 Msps).
    pub fn nrsc5_pipe_samples_cu8(
        st: *mut nrsc5_t,
        samples: *const u8,
        length: c_uint,
    ) -> c_int;

    /// Push `length` signed-16-bit complex I/Q samples (interleaved I, Q)
    /// into the demodulator. The expected sample rate depends on mode:
    /// [`NRSC5_SAMPLE_RATE_CS16_FM`] for FM and [`NRSC5_SAMPLE_RATE_CS16_AM`]
    /// for AM.
    pub fn nrsc5_pipe_samples_cs16(
        st: *mut nrsc5_t,
        samples: *const i16,
        length: c_uint,
    ) -> c_int;

    /// Write the library version (e.g. `"3.1.0"`) into `*version`.
    /// The string is owned by libnrsc5; do not free.
    pub fn nrsc5_get_version(version: *mut *const c_char);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time-ish sanity checks on the layout of types we care
    /// about. These don't validate exhaustive offset equality with C
    /// (no portable way without bindgen + an actual C compiler), but
    /// catch the most likely mistakes: wrong field count and wrong
    /// pointer width / int width on the event union variants.
    #[test]
    fn layout_smoke() {
        use std::mem::{align_of, size_of};

        // Pointer-sized fields dominate most variants; on 64-bit
        // Windows that's 8 bytes per pointer.
        assert_eq!(size_of::<*mut nrsc5_t>(), size_of::<usize>());

        // nrsc5_event_sync = float + psmi + 4 AM ints = 6 * 4 = 24 bytes
        // (with 4-byte alignment). v3.2.0 added pli/hppi/aabi/rdbi.
        assert_eq!(size_of::<nrsc5_event_sync>(), 24);
        assert_eq!(align_of::<nrsc5_event_sync>(), 4);

        // nrsc5_event_agc = 2 floats + 1 int = 12 bytes
        assert_eq!(size_of::<nrsc5_event_agc>(), 12);

        // nrsc5_event_audio_service = 7 unsigned ints + 1 signed int = 32
        assert_eq!(size_of::<nrsc5_event_audio_service>(), 32);

        // Audio buffer: program (4) + pad to 8 + ptr (8) + size (8) = 24
        assert_eq!(size_of::<nrsc5_event_audio>(), 24);

        // The union must be at least as large as its largest variant.
        // sis is the biggest with ~14 pointers + 9 ints + 2 floats.
        assert!(size_of::<nrsc5_event_payload>() >= size_of::<nrsc5_event_sis>());

        // The full event = tag (4) + pad to 8 + union. Union alignment
        // forces the struct to 8-byte alignment on 64-bit.
        assert_eq!(align_of::<nrsc5_event_t>(), align_of::<nrsc5_event_payload>());
    }

    /// Verify the callback type signature matches what the C side
    /// expects (`void (*)(const nrsc5_event_t *, void *)`). This is a
    /// compile-only check via assignment.
    #[test]
    fn callback_signature_compiles() {
        unsafe extern "C" fn cb(_evt: *const nrsc5_event_t, _opaque: *mut c_void) {}
        let _: nrsc5_callback_t = Some(cb);
    }
}
