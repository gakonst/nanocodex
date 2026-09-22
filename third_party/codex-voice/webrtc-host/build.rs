fn main() {
    println!("cargo:rerun-if-env-changed=STABLE_GIT_COMMIT");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        // Retain WebRTC's Objective-C categories in the static archive.
        println!("cargo:rustc-link-arg=-Wl,-ObjC");
    }
    webrtc_sys_build::download_webrtc().expect("libWebRTC native archive");
    // Package the license inventory from the exact native archive being linked.
    let license = webrtc_sys_build::webrtc_dir().join("LICENSE.md");
    assert!(license.is_file(), "missing libWebRTC license inventory");
    assert_eq!(webrtc_sys_build::WEBRTC_TAG, "webrtc-89d790b");
    println!("cargo:rerun-if-changed={}", license.display());
    println!(
        "cargo:rustc-env=NANOCODEX_WEBRTC_LICENSE={}",
        license.display()
    );
}
