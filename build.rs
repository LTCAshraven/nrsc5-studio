use std::env;
use std::path::Path;

fn main() {
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
