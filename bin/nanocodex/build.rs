mod build_version;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=build_version.rs");
    build_version::emit()?;
    build_hand_menu_bar()
}

fn build_hand_menu_bar() -> Result<(), Box<dyn std::error::Error>> {
    use std::{env, path::PathBuf, process::Command};
    let source =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("CARGO_MANIFEST_DIR is unset")?)
            .join("../../macos/HandMenuBar/main.swift");
    println!("cargo:rerun-if-changed={}", source.display());
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return Ok(());
    }
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
        .arg(&source)
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
