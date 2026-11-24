//! Ubuntu machines without the `libxkbcommon-x11-dev` package only ship `libxkbcommon-x11.so.0`,
//! and the linker looks for `libxkbcommon-x11.so`. Without sudo we can't install the dev package,
//! so point the linker at a symlink in `OUT_DIR` (which lives on sgoinfre with the rest of the build).

use std::{env, path::PathBuf};

const LIB_DIRS: &[&str] = &["/usr/lib/x86_64-linux-gnu", "/usr/lib64", "/usr/lib"];
const SHIMS: &[&str] = &["xkbcommon-x11", "xkbcommon"];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo::rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return Ok(());
    }
    let out = PathBuf::from(env::var("OUT_DIR")?).join("syslibs");
    std::fs::create_dir_all(&out)?;
    let mut shimmed = false;
    for name in SHIMS {
        let dev = format!("lib{name}.so");
        if LIB_DIRS.iter().any(|d| PathBuf::from(d).join(&dev).exists()) {
            continue; // dev package installed, nothing to do
        }
        let Some(runtime) = LIB_DIRS
            .iter()
            .map(|d| PathBuf::from(d).join(format!("{dev}.0")))
            .find(|p| p.exists())
        else {
            println!("cargo::warning=neither {dev} nor {dev}.0 found; linking will fail");
            continue;
        };
        let link = out.join(&dev);
        if std::fs::symlink_metadata(&link).is_ok() {
            std::fs::remove_file(&link)?;
        }
        std::os::unix::fs::symlink(&runtime, &link)?;
        shimmed = true;
    }
    if shimmed {
        println!("cargo::rustc-link-search=native={}", out.display());
    }
    Ok(())
}
