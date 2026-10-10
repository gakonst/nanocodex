//! Native helpers that Cargo cannot express as dependencies. Release
//! provenance (commit, tag, Hand identity) is read by the executable entry
//! points instead, and the Linux screen-helper payload is embedded by the
//! `embedded-screen-helpers` feature, so this script reruns only when the
//! macOS menu-bar source changes.

use std::{env, path::PathBuf, process::Command};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("CARGO_MANIFEST_DIR is unset")?)
            .join("../../macos/HandMenuBar/main.swift");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", source.display());
    let target_os = env::var("CARGO_CFG_TARGET_OS")?;
    if target_os == "macos" {
        build_hand_menu_bar(&source)?;
    }
    Ok(())
}

/// Compiles the standalone Hand menu-bar helper that the macOS Hand embeds.
fn build_hand_menu_bar(source: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let architecture = match env::var("CARGO_CFG_TARGET_ARCH")?.as_str() {
        "aarch64" => "arm64",
        "x86_64" => "x86_64",
        other => return Err(format!("Unsupported macOS menu bar architecture: {other}").into()),
    };
    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR is unset")?)
        .join("nanocodex-hand-menu-bar");
    let status = Command::new("xcrun")
        .args([
            "swiftc",
            "-O",
            "-target",
            &format!("{architecture}-apple-macosx14.0"),
            "-framework",
            "AppKit",
        ])
        .arg(source)
        .arg("-o")
        .arg(&output)
        .status()?;
    if !status.success() {
        return Err("Could not compile the standalone Hand menu bar helper".into());
    }
    let status = Command::new("/usr/bin/codesign")
        .args([
            "--force",
            "--sign",
            "-",
            "--identifier",
            "com.nanocodex.hand-menu-bar",
        ])
        .arg(&output)
        .status()?;
    if !status.success() {
        return Err("Could not sign the standalone Hand menu bar helper".into());
    }
    Ok(())
}
