//! Lightweight RTL-SDR presence probe.
//!
//! We dynamically load `librtlsdr.dll` (the same DLL nrsc5.exe is linked
//! against, shipped alongside it in `bin/`) and call
//! `rtlsdr_get_device_count()`. That's the canonical way every librtlsdr
//! consumer asks "is there an SDR plugged in?" — it walks libusb's device
//! list, which is fast (single-digit ms when no devices are attached).
//!
//! We intentionally do *not* spawn nrsc5 just to probe: spawning a child
//! every couple of seconds would show up in Task Manager, briefly grab the
//! USB device, and conflict with anything else trying to use it.
//!
//! If the DLL is missing or the symbol can't be resolved (e.g. a future
//! librtlsdr ABI break), the probe returns `None` rather than `Some(0)` so
//! callers can distinguish "no SDR" from "we don't know" and avoid showing
//! a misleading no-SDR overlay.

use std::path::PathBuf;
use std::sync::OnceLock;

/// Locate `librtlsdr.dll` next to the running executable or the workspace
/// `bin/` folder. Mirrors `find_nrsc5_exe()` in `ffi/mod.rs` so a dev build
/// run from the workspace root resolves the same DLL the shipped builds
/// load.
#[cfg(target_os = "windows")]
fn find_librtlsdr() -> Option<PathBuf> {
    const DLL: &str = "librtlsdr.dll";
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("bin").join(DLL);
            if p.exists() {
                return Some(p);
            }
            let p = dir.join(DLL);
            if p.exists() {
                return Some(p);
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        let p = cwd.join("bin").join(DLL);
        if p.exists() {
            return Some(p);
        }
        let p = cwd.join(DLL);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

#[cfg(not(target_os = "windows"))]
fn find_librtlsdr() -> Option<PathBuf> {
    // Try portable/dev-local copies first, then fall back to the
    // system dynamic loader search path by soname.
    const CANDIDATES: [&str; 4] = [
        "librtlsdr.so",
        "librtlsdr.so.0",
        "librtlsdr.so.2",
        "librtlsdr.dylib",
    ];

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for name in CANDIDATES {
                let p = dir.join("bin").join(name);
                if p.exists() {
                    return Some(p);
                }
                let p = dir.join(name);
                if p.exists() {
                    return Some(p);
                }
            }
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        for name in CANDIDATES {
            let p = cwd.join("bin").join(name);
            if p.exists() {
                return Some(p);
            }
            let p = cwd.join(name);
            if p.exists() {
                return Some(p);
            }
        }
    }

    // Let dlopen resolve via ld.so/dyld search paths.
    Some(PathBuf::from("librtlsdr.so.0"))
}

type RtlsdrGetDeviceCount = unsafe extern "C" fn() -> u32;

/// Loaded `librtlsdr.dll` plus the cached function pointer. The `Library`
/// is kept alive for the lifetime of the process so the function pointer
/// stays valid.
struct ProbeLib {
    _lib: libloading::Library,
    get_count: RtlsdrGetDeviceCount,
}

// SAFETY: `libloading::Library` is `Send + Sync` on Windows, and raw
// `extern "C"` function pointers are `Send + Sync`. `rtlsdr_get_device_count`
// itself is internally protected by libusb's locking.
unsafe impl Send for ProbeLib {}
unsafe impl Sync for ProbeLib {}

fn lib() -> Option<&'static ProbeLib> {
    static LIB: OnceLock<Option<ProbeLib>> = OnceLock::new();
    LIB.get_or_init(|| {
        let path = find_librtlsdr()?;
        // SAFETY: load with `LOAD_WITH_ALTERED_SEARCH_PATH` (0x8) so
        // `librtlsdr.dll`'s own directory (e.g. `bin/`) is the search
        // base for its dependencies. Otherwise Windows looks in the
        // calling process's directory, which doesn't contain the
        // sibling `libusb-1.0.dll` that modern librtlsdr builds
        // dynamically link against.
        let library = unsafe {
            #[cfg(target_os = "windows")]
            {
                const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 0x0000_0008;
                libloading::os::windows::Library::load_with_flags(
                    &path,
                    LOAD_WITH_ALTERED_SEARCH_PATH,
                )
                .ok()
                .map(libloading::Library::from)?
            }
            #[cfg(not(target_os = "windows"))]
            {
                libloading::Library::new(&path).ok()?
            }
        };
        // SAFETY: `rtlsdr_get_device_count` is a stable librtlsdr export
        // with the documented signature `uint32_t (*)(void)`.
        let get_count: RtlsdrGetDeviceCount = unsafe {
            let sym: libloading::Symbol<RtlsdrGetDeviceCount> =
                library.get(b"rtlsdr_get_device_count\0").ok()?;
            *sym
        };
        Some(ProbeLib {
            _lib: library,
            get_count,
        })
    })
    .as_ref()
}

/// Returns the number of attached RTL-SDR devices, or `None` if the probe
/// itself failed (DLL missing on this system, or symbol couldn't be
/// resolved). Callers should treat `None` as "unknown" — *not* zero — and
/// avoid showing a "no SDR detected" UI in that case to prevent false
/// positives on systems where the probe isn't available.
pub fn device_count() -> Option<u32> {
    let l = lib()?;
    // SAFETY: `rtlsdr_get_device_count` takes no arguments, has no
    // preconditions, and returns by value. The DLL has been kept loaded
    // since `lib()` first succeeded.
    Some(unsafe { (l.get_count)() })
}

/// Drivers we consider "an SDR" for the purposes of the no-SDR overlay.
/// Mirror of `sdr::soapy::SoapySdr::SUPPORTED_DRIVERS` — duplicated here
/// rather than re-exported because this presence probe runs every 2 s and
/// we want it to stay independent of the full enumeration code path.
const SOAPY_SUPPORTED_DRIVERS: &[&str] = &[
    "rtlsdr", "sdrplay", "airspy", "hackrf", "lime", "plutosdr", "remote",
];

/// Returns the number of SDRs visible to libSoapySDR whose driver is in
/// `SOAPY_SUPPORTED_DRIVERS`, or `None` if the enumeration itself
/// failed (e.g. a Soapy module panicked during its find function).
///
/// **Cost:** one `soapysdr::enumerate("")` call. Empty USB bus returns
/// in under 100 ms on Windows; an SDRplay-only setup typically returns
/// in well under that. Cheap enough to call once per 2 s probe tick on
/// the cold path (when the librtlsdr probe says zero), but the caller
/// should still avoid running it during an active stream to prevent
/// USB contention with the live device.
///
/// Used by the no-SDR overlay so an SDRplay (or any non-RTL Soapy
/// device) is correctly recognized as "an SDR is present" \u2014 the
/// librtlsdr probe alone misses these.
pub fn soapy_supported_count() -> Option<u32> {
    match soapysdr::enumerate("") {
        Ok(devices) => {
            let n = devices
                .iter()
                .filter(|args| {
                    args.get("driver")
                        .map(|d| {
                            SOAPY_SUPPORTED_DRIVERS
                                .iter()
                                .any(|s| d.eq_ignore_ascii_case(s))
                        })
                        .unwrap_or(false)
                })
                .count() as u32;
            Some(n)
        }
        Err(_) => None,
    }
}

/// Coarse state of the Windows `SDRplayAPIService` SCM service.
///
/// The SDRplay API runs as a Windows service that the libSoapySDR
/// `sdrplay` driver talks to on behalf of every client. When the
/// service is stopped, `SoapySDRDevice_make("driver=sdrplay")` either
/// hangs (waiting for the named pipe) or returns a confusing
/// "device not found" error — and any *cached* enumeration result
/// from when the service *was* running gets stale, so the app keeps
/// thinking SDRplay is available.
///
/// Querying the service state is the cheap, reliable signal: an
/// unprivileged user can call `OpenSCManager(SC_MANAGER_CONNECT)` +
/// `OpenService(SERVICE_QUERY_STATUS)` (or, in our case, `sc.exe
/// query`) without admin rights. Starting / stopping the service
/// *would* need admin (`SERVICE_START` / `SERVICE_STOP`), so we
/// don't try — we just surface the state so the user can flip it via
/// Services.msc themselves.
#[cfg(target_os = "windows")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdrplayServiceState {
    /// Service exists and is currently running. SDRplay-via-Soapy
    /// should work.
    Running,
    /// Service exists and is fully stopped. SDRplay opens will fail
    /// fast or hang; surface to the user with a "please start the
    /// service" hint.
    Stopped,
    /// Service exists and is in the middle of starting or stopping.
    /// Transient; the next probe tick should observe a settled state.
    Pending,
    /// Service exists but is in an unexpected state (paused, etc.).
    /// Treat as not-running for UI purposes.
    Other,
}

#[cfg(not(target_os = "windows"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// Placeholder so the type exists on non-Windows targets; the probe below always
// returns `None`, so no variant is ever constructed off-Windows.
#[allow(dead_code)]
pub enum SdrplayServiceState {
    /// Placeholder so the type compiles on non-Windows targets. The
    /// SDRplay API is Windows-only in practice; on Linux the probe
    /// always returns `None`.
    Running,
    Stopped,
    Pending,
    Other,
}

/// Probe the Windows `SDRplayAPIService` state without elevation.
///
/// Returns:
/// * `None` — the service isn't installed on this machine (no SDRplay
///   API ever installed, or wiped). Treated as "SDRplay support not
///   available"; the UI says nothing about it.
/// * `Some(Running)` — green-light SDRplay enumeration / open.
/// * `Some(Stopped | Pending | Other)` — SDRplay support is present
///   but currently unusable. UI shows an actionable hint.
///
/// Implementation note: we shell out to `sc.exe query` (a stock
/// Windows tool that ships in every supported SKU) rather than pull
/// in `windows-sys` just for `OpenSCManager` / `QueryServiceStatus`.
/// One subprocess per ~2 s probe tick is well within the budget and
/// avoids growing the dependency graph. `CREATE_NO_WINDOW` keeps the
/// transient console invisible.
#[cfg(target_os = "windows")]
pub fn sdrplay_service_state() -> Option<SdrplayServiceState> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const SDRPLAY_SERVICE_NAME: &str = "SDRplayAPIService";
    let out = Command::new("sc.exe")
        .args(["query", SDRPLAY_SERVICE_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !out.status.success() {
        // sc returns exit 1060 ("service does not exist") when the
        // service isn't installed. Treat any non-success as "no
        // SDRplay API installed" — we don't want to surface false
        // positives.
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        // The line we want looks like:
        //   "        STATE              : 4  RUNNING"
        // Match on the "STATE" key (LHS of the first colon) so we
        // don't confuse it with other lines that happen to contain
        // the word.
        let trimmed = line.trim_start();
        if let Some(idx) = trimmed.find(':') {
            if trimmed[..idx].trim_end() != "STATE" {
                continue;
            }
            let rhs = trimmed[idx + 1..].trim();
            // RHS is e.g. "4  RUNNING" — the second whitespace token
            // is the human-readable state word.
            let word = rhs.split_whitespace().nth(1).unwrap_or("");
            return Some(match word {
                "RUNNING" => SdrplayServiceState::Running,
                "STOPPED" => SdrplayServiceState::Stopped,
                "START_PENDING" | "STOP_PENDING" => SdrplayServiceState::Pending,
                _ => SdrplayServiceState::Other,
            });
        }
    }
    None
}

#[cfg(not(target_os = "windows"))]
pub fn sdrplay_service_state() -> Option<SdrplayServiceState> {
    // SDRplay API is Windows-only; no analog on Linux/macOS.
    None
}
