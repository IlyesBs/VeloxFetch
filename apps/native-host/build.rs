//! Link the Visual C++ runtime statically on Windows (MSVC), as `tauri-build` does for the desktop app.
//!
//! Browsers start this program on their own; on a computer without the "Visual C++ Redistributable"
//! a dynamically linked host cannot start at all (VCRUNTIME140.dll missing) and the extension only
//! sees a broken link. The Universal CRT stays dynamic: it is part of Windows 10 and later.
//!
//! Adapted from tauri-build's `static_vcruntime.rs` (MIT / Apache-2.0), itself taken from
//! <https://github.com/ChrisDenton/static_vcruntime/>.

use std::io::Write;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if !msvc {
        return;
    }
    override_msvcrt_lib();
    // Disable conflicting libraries that aren't hard coded by Rust.
    for lib in ["libvcruntimed", "vcruntime", "vcruntimed", "libcmtd", "msvcrt", "msvcrtd", "libucrt", "libucrtd"] {
        println!("cargo:rustc-link-arg-bins=/NODEFAULTLIB:{lib}.lib");
    }
    // The ones we want.
    for lib in ["libcmt", "libvcruntime", "ucrt"] {
        println!("cargo:rustc-link-arg-bins=/DEFAULTLIB:{lib}.lib");
    }
}

/// Override the msvcrt.lib Rust names by a (mostly) empty object file of the same name.
fn override_msvcrt_lib() {
    let machine: &[u8] = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => &[0x64, 0x86],
        Ok("x86") => &[0x4C, 0x01],
        _ => return,
    };
    let bytes: &[u8] = &[
        1, 0, 94, 3, 96, 98, 60, 0, 0, 0, 1, 0, 0, 0, 0, 0, 132, 1, 46, 100, 114, 101, 99, 116, 118, 101, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 10, 16, 0, 46, 100, 114, 101, 99, 116, 118, 101, 0, 0, 0, 0, 1, 0, 0, 0, 3,
        0, 4, 0, 0, 0,
    ];
    let Ok(out_dir) = std::env::var("OUT_DIR") else { return };
    let path = Path::new(&out_dir).join("msvcrt.lib");
    if let Ok(mut f) = std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
        let _ = f.write_all(machine).and_then(|_| f.write_all(bytes));
    }
    println!("cargo:rustc-link-search=native={out_dir}");
}
