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
