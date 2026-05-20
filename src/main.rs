// In release builds, mark the binary as a Windows GUI app so no console
// window appears when the user double-clicks the .exe. Debug builds keep the
// console so `println!` / `eprintln!` remain visible during development.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod art_cache;
mod collage;
mod config;
mod dsp;
mod ffi;
mod gui;
mod icon;
mod maps;
mod paths;
mod play_log;
mod sdr;
mod sdr_detect;
#[cfg(target_os = "windows")]
mod winaudio;

fn main() -> eframe::Result<()> {
    // Self-locate bundled native DLLs so the user never has to
    // shell-set PATH or SOAPY_SDR_PLUGIN_PATH. In a portable install
    // (or a `cargo run` from the repo root) the layout is:
    //
    //   <exe_dir>\bin\libSoapySDR.dll
    //   <exe_dir>\bin\librtlsdr.dll
    //   <exe_dir>\bin\SoapySDR\modules0.8\SoapyRTLSDR.dll
    //   <exe_dir>\bin\SoapySDR\modules0.8\libsdrPlaySupport.dll
    //
    // We prepend the DLL directory to PATH so the OS loader finds
    // libSoapySDR.dll on demand, and set SOAPY_SDR_PLUGIN_PATH so
    // libSoapySDR itself finds the per-driver module DLLs. Both are
    // best-effort: when `bin\` isn't present we trust whatever the
    // current shell already has on PATH (developer with system-wide
    // SoapySDR, for example).
    install_bundled_dll_paths();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id("nrsc5-studio")
            // Default first-launch geometry. Logical (DPI-aware) pixels, so
            // it looks consistently sized regardless of monitor DPI. eframe
            // clamps this to the available work area on smaller monitors,
            // and `persist_window: true` means user resizes stick afterward.
            .with_inner_size([1623.0, 1179.0])
            .with_min_inner_size([960.0, 600.0])
            .with_icon(icon::build_window_icon()),
        persist_window: true,
        // In portable mode, route eframe's persistence (window geometry +
        // serialized dock layout) into `<exe_dir>\data\eframe\app.ron` so
        // the bundle leaves no traces on the host. `None` falls back to
        // eframe's default location under `%APPDATA%`.
        persistence_path: paths::eframe_storage_file(),
        ..Default::default()
    };
    eframe::run_native(
        concat!("NRSC5 Studio ", env!("CARGO_PKG_VERSION")),
        options,
        Box::new(|cc| Ok(Box::new(app::Nrsc5App::new(cc)))),
    )
}

/// Resolve `<exe_dir>\bin\` (and the SoapySDR module subdir) at
/// startup and surface them through environment variables so the
/// OS DLL loader and libSoapySDR find the bundled binaries without
/// any user configuration. Silent best-effort — if the bundle isn't
/// present we leave PATH alone and assume DLLs are already resolvable
/// (developer with a system-wide SoapySDR / RTL-SDR install).
fn install_bundled_dll_paths() {
    // Collect every directory we want to prepend to PATH. Order
    // matters: earlier entries take precedence when Windows resolves
    // a DLL name. We push them in *reverse* prepend order (so the
    // last one pushed ends up first on PATH).
    let mut prepend: Vec<std::ffi::OsString> = Vec::new();

    // 1) <exe_dir>\bin\ -- our bundled libSoapySDR.dll, librtlsdr,
    //    libusb, libao, nrsc5.exe, etc. Mandatory for the portable
    //    install to work at all.
    if let Some(bin) = paths::bundled_dll_dir() {
        prepend.push(bin.into_os_string());
    }

    // 2) SDRplay API runtime directory. The SDRplay API installer
    //    (sdrplay.com) drops sdrplay_api.dll into
    //    `C:\Program Files\SDRplay\API\x64\` (x64 build) but it does
    //    NOT reliably add that directory to the system-wide PATH on
    //    every Windows configuration -- which means our bundled
    //    `libsdrPlaySupport.dll` (the SoapySDR plugin) cannot find
    //    `sdrplay_api.dll` at load time, even though the API service
    //    is running. We detect the standard install location ourselves
    //    so users don't have to manually edit PATH.
    //
    //    We add both candidate paths if they exist; the API installer
    //    historically placed the runtime under different sub-paths
    //    across major versions. Skipping anything non-existent keeps
    //    PATH clean.
    for candidate in [
        r"C:\Program Files\SDRplay\API\x64",
        r"C:\Program Files (x86)\SDRplay\API\x86",
    ] {
        let p = std::path::Path::new(candidate);
        if p.is_dir() && p.join("sdrplay_api.dll").is_file() {
            prepend.push(p.as_os_str().to_os_string());
        }
    }

    if !prepend.is_empty() {
        // Prepend (don't replace) so a user's existing PATH entries
        // still resolve. `set_var` here is sound because we run
        // before any other thread is spawned.
        let prev_path = std::env::var_os("PATH").unwrap_or_default();
        let mut new_path = std::ffi::OsString::new();
        for (i, dir) in prepend.iter().enumerate() {
            if i > 0 {
                new_path.push(";");
            }
            new_path.push(dir);
        }
        new_path.push(";");
        new_path.push(&prev_path);
        // SAFETY: single-threaded startup. No other code is reading
        // PATH yet. Pre-Rust-1.78 set_var is safe here unconditionally;
        // newer toolchains mark it unsafe in multi-threaded contexts
        // only -- main thread before eframe::run_native is fine.
        unsafe {
            std::env::set_var("PATH", new_path);
        }
    }

    if let Some(modules) = paths::bundled_soapy_modules_dir() {
        unsafe {
            std::env::set_var("SOAPY_SDR_PLUGIN_PATH", modules.as_os_str());
        }
    }
}
