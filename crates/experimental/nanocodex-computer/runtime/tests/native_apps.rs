#![cfg(target_os = "macos")]
use skyre::native::{App, macos::apps::discover};
#[test]
fn installed_bundle_discovery_reads_owned_metadata_without_launch_and_merges_running() {
    let directory = tempfile::tempdir().unwrap();
    let bundle = directory.path().join("Synthetic.app");
    std::fs::create_dir_all(bundle.join("Contents")).unwrap();
    std::fs::write(bundle.join("Contents/Info.plist"),r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>CFBundleIdentifier</key><string>org.skyre.synthetic.metadata</string>
<key>CFBundleName</key><string>Synthetic Metadata</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>"#).unwrap();
    let rows = discover(&[directory.path().into()], vec![]).unwrap();
    assert_eq!(rows[0]["bundleIdentifier"], "org.skyre.synthetic.metadata");
    assert_eq!(rows[0]["displayName"], "Synthetic Metadata");
    assert_eq!(rows[0]["isRunning"], false);
    let rows = discover(
        &[directory.path().into()],
        vec![App {
            window_id: None,
            id: "org.skyre.synthetic.metadata".into(),
            name: "Synthetic Running".into(),
            path: bundle.to_str().unwrap().into(),
            pid: 123,
        }],
    )
    .unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["isRunning"], true);
    assert!(rows[0].get("useCount").is_none());
    assert!(!bundle.join("Contents/MacOS").exists());
}
