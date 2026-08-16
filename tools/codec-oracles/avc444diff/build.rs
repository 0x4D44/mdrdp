//! Compile `shim.c` against the installed FreeRDP 3 headers and link the library.
//!
//! `pkg-config` is asked first, because that is how `tools/codec-oracles/refdec.c`
//! is documented to build. The Homebrew layout is the fallback, and is the only
//! layout this oracle has actually been run against.

use std::process::Command;

fn pkg_config(args: &[&str]) -> Option<Vec<String>> {
    let out = Command::new("pkg-config").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .map(str::to_owned)
            .collect(),
    )
}

fn main() {
    println!("cargo:rerun-if-changed=shim.c");
    println!("cargo:rerun-if-changed=build.rs");

    let mut build = cc::Build::new();
    build.file("shim.c");

    let cflags = pkg_config(&["--cflags", "freerdp3", "winpr3"]);
    let libs = pkg_config(&["--libs", "freerdp3", "winpr3"]);

    match (cflags, libs) {
        (Some(cflags), Some(libs)) => {
            for flag in cflags {
                match flag.strip_prefix("-I") {
                    Some(dir) => {
                        build.include(dir);
                    }
                    None => {
                        build.flag(&flag);
                    }
                }
            }
            for lib in libs {
                if let Some(dir) = lib.strip_prefix("-L") {
                    println!("cargo:rustc-link-search=native={dir}");
                } else if let Some(name) = lib.strip_prefix("-l") {
                    println!("cargo:rustc-link-lib=dylib={name}");
                }
            }
        }
        _ => {
            build
                .include("/opt/homebrew/include/freerdp3")
                .include("/opt/homebrew/include/winpr3");
            println!("cargo:rustc-link-search=native=/opt/homebrew/lib");
            println!("cargo:rustc-link-lib=dylib=freerdp3");
            println!("cargo:rustc-link-lib=dylib=winpr3");
        }
    }

    build.compile("avc444shim");
}
