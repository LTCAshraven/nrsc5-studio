//! Library crate exposing the SDR backend abstraction for examples and
//! integration tests. The binary entry point lives in `src/main.rs` and
//! consumes its own `mod sdr;` privately; this lib re-exposes the
//! same module so `examples/*.rs` (and future integration tests under
//! `tests/`) can call into `SoapySdr` and the `Sdr` trait without
//! duplicating their source via `#[path]` hacks.
//!
//! The duplicate compilation cost (sdr/* compiles once for the lib and
//! once for the bin) is negligible — sdr/ is a few hundred lines and
//! incremental builds are unaffected. The alternative (promoting the
//! whole binary to a lib + thin main shim) is a much bigger refactor
//! that v0.3.0 doesn't need to take on.

pub mod paths;
pub mod sdr;
