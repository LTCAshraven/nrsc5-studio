use std::env;
use std::path::{Path, PathBuf};

#[path = "src/icon_render.rs"]
mod icon_render;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/icon_render.rs");

    // Embed the application icon as a Windows resource. Skips silently on
    // non-Windows targets and warns (rather than failing the build) if the
    // resource compiler is missing.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        ensure_bundled_toolchain_on_path();
        embed_app_icon();
        // Phase 2: the safe wrapper in src/ffi/api.rs declares
        // `#[link(name = "nrsc5")]`. Tell the linker where to find
        // `libnrsc5.dll` so the import resolution succeeds once any
        // wrapper method is referenced (Phase 3 cutover wires real
        // callers; until then the linker GCs unused externs).
        emit_nrsc5_link_search();
    }

    // Optional bindgen generation, gated by NRSC5_GENERATE_BINDINGS.
    generate_bindings();
}

/// Emit `cargo:rustc-link-search` pointing at the workspace `bin/`
/// directory so the LLD-link import-library lookup for `libnrsc5.dll`
/// (and any other bundled DLL we may add later) can succeed. Idempotent
/// and silent — only emits if `bin/` exists.
fn emit_nrsc5_link_search() {
    let Some(manifest_dir) = env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from) else {
        return;
    };
    let bin = manifest_dir.join("bin");
    if bin.is_dir() {
        println!("cargo:rustc-link-search=native={}", bin.display());
        // Re-run if a DLL is added/removed from bin/.
        println!("cargo:rerun-if-changed={}", bin.display());
    }
}

/// If the project's bundled llvm-mingw toolchain is present, prepend its
/// `bin/` directory to PATH and create the `windres.exe` / `dlltool.exe`
/// aliases that some host-tool code looks for under their non-prefixed
/// names. Makes plain `cargo build` / `cargo run` from the workspace root
/// "just work" without manual PATH setup or going through cargo-gnu.ps1.
fn ensure_bundled_toolchain_on_path() {
    let Some(manifest_dir) = env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from) else {
        return;
    };
    let bin = manifest_dir
        .join(".toolchains")
        .join("llvm-mingw-20260505-ucrt-x86_64")
        .join("bin");
    if !bin.is_dir() {
        return;
    }

    // Create canonical aliases for tools that embed_resource and similar
    // host-side helpers look up by their unprefixed names. llvm-mingw ships
    // them as `x86_64-w64-mingw32-{tool}.exe`; copy a same-named alias the
    // first time we're invoked. Failures are non-fatal — the worst case is
    // the user falls back to cargo-gnu.ps1.
    for (alias, source) in [
        ("windres.exe", "x86_64-w64-mingw32-windres.exe"),
        ("dlltool.exe", "x86_64-w64-mingw32-dlltool.exe"),
    ] {
        let alias_path = bin.join(alias);
        let source_path = bin.join(source);
        if !alias_path.exists() && source_path.exists() {
            let _ = std::fs::copy(&source_path, &alias_path);
        }
    }

    // Prepend the bin dir to PATH for the rest of this build.rs invocation
    // (and for any child processes embed_resource spawns).
    let current_path = env::var_os("PATH").unwrap_or_default();
    let mut paths: Vec<PathBuf> = vec![bin.clone()];
    paths.extend(env::split_paths(&current_path));
    if let Ok(new_path) = env::join_paths(paths) {
        env::set_var("PATH", new_path);
    }
}

fn embed_app_icon() {
    let Some(out_dir) = env::var_os("OUT_DIR").map(PathBuf::from) else {
        println!("cargo:warning=OUT_DIR not set; skipping icon embedding");
        return;
    };

    // Render a multi-resolution .ico file from the shared icon-rendering
    // code. Multiple resolutions let Windows pick the closest size for any
    // display context (Explorer small/medium/large icons, taskbar, Alt-Tab,
    // pinned shortcuts, etc.) instead of downscaling a single large image.
    let ico_path = out_dir.join("nrsc5-studio.ico");
    let sizes: [u32; 6] = [16, 32, 48, 64, 128, 256];
    let mut icon_dir = ico::IconDir::new(ico::ResourceType::Icon);
    for size in sizes {
        let img = icon_render::render(size);
        let image = ico::IconImage::from_rgba_data(size, size, img.into_raw());
        match ico::IconDirEntry::encode(&image) {
            Ok(entry) => icon_dir.add_entry(entry),
            Err(err) => {
                println!("cargo:warning=failed to encode {size}x{size} icon: {err}");
                return;
            }
        }
    }
    let file = match std::fs::File::create(&ico_path) {
        Ok(f) => f,
        Err(err) => {
            println!(
                "cargo:warning=failed to create {}: {err}",
                ico_path.display()
            );
            return;
        }
    };
    if let Err(err) = icon_dir.write(file) {
        println!("cargo:warning=failed to write icon file: {err}");
        return;
    }

    // Generate a minimal Windows resource script and hand it to
    // embed-resource, which invokes `rc.exe` (MSVC) or `windres` (MinGW /
    // llvm-mingw) to produce a linkable resource object.
    let rc_path = out_dir.join("nrsc5-studio.rc");
    let rc_contents = format!(
        "1 ICON \"{}\"\n",
        ico_path.to_string_lossy().replace('\\', "/")
    );
    if let Err(err) = std::fs::write(&rc_path, rc_contents) {
        println!("cargo:warning=failed to write {}: {err}", rc_path.display());
        return;
    }

    let result = embed_resource::compile(&rc_path, embed_resource::NONE);
    if let Err(err) = result.manifest_optional() {
        println!(
            "cargo:warning=embedding application icon failed (the .exe will \
             still build, just without a custom icon resource): {err}"
        );
    }
}

fn generate_bindings() {
    let header = Path::new("res/nrsc5.h");
    println!("cargo:rerun-if-changed={}", header.display());

    if !header.exists() {
        println!("cargo:warning=res/nrsc5.h not found; skipping bindgen generation");
        return;
    }

    let enabled = env::var("NRSC5_GENERATE_BINDINGS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    if !enabled {
        println!("cargo:rerun-if-env-changed=NRSC5_GENERATE_BINDINGS");
        return;
    }

    let bindings = std::panic::catch_unwind(|| {
        bindgen::Builder::default()
            .header(header.to_string_lossy())
            .allowlist_function("nrsc5_.*")
            .allowlist_type("nrsc5_.*")
            .allowlist_var("NRSC5_.*")
            .derive_default(true)
            .generate()
    });

    match bindings {
        Ok(Ok(output)) => {
            let out_dir = env::var("OUT_DIR").unwrap_or_else(|_| "target".to_string());
            let out_file = Path::new(&out_dir).join("nrsc5_bindings.rs");
            if let Err(err) = output.write_to_file(&out_file) {
                println!("cargo:warning=failed to write bindgen output: {err}");
            }
        }
        Ok(Err(err)) => {
            println!("cargo:warning=bindgen generation failed: {err}");
        }
        Err(_) => println!(
            "cargo:warning=bindgen panicked (likely missing libclang); continuing without generated bindings"
        ),
    }
}
