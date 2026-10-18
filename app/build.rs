//! Embeds the application icon into the Windows executable so Explorer and the taskbar show it.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=../packaging/windows/mkres.py");
    println!("cargo:rerun-if-changed=../packaging/icon.py");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let res = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("nullamp.res");
    let script = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../packaging/windows/mkres.py");
    // The generator is plain Python; without it the exe still works, it just has the generic icon.
    let made = ["python3", "python"].iter().any(|py| Command::new(py).arg(&script).arg(&res).status().is_ok_and(|s| s.success()));
    if made {
        println!("cargo:rustc-link-arg-bins={}", res.display());
    } else {
        println!("cargo:warning=python not found, building the Windows exe without an embedded icon");
    }
}
