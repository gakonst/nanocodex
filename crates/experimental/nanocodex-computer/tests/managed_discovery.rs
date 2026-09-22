#![cfg(target_os = "linux")]

use nanocodex_computer::ComputerConfig;
use std::{fs, process::Command};

// Run discovery in a child so environment changes cannot race other tests.
#[test]
fn discovery_child() {
    let Some(expected) = std::env::var_os("TEST_EXPECTED_PROVIDER") else {
        return;
    };
    let actual = ComputerConfig::discover();
    if expected == "absent" {
        assert!(actual.is_none());
    } else {
        let actual = actual.unwrap();
        assert_eq!(actual.executable.as_os_str(), expected);
        assert!(actual.args.is_empty());
        assert!(actual.environment.is_empty());
    }
}

#[test]
fn linux_managed_launcher_discovery_respects_explicit_selection() {
    let root = std::env::temp_dir().join(format!(
        "nanocodex-discovery-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(root.join("runtimes/openai-cua")).unwrap();
    let receipt = root.join("runtimes/openai-cua/provider.json");
    let launcher = root.join("upstream/cua-provider");
    let check = |explicit: Option<&str>, expected: &std::ffi::OsStr| {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "discovery_child", "--nocapture"])
            .env("NANOCODEX_DIR", &root)
            .env("TEST_EXPECTED_PROVIDER", expected)
            .env_remove("NANOCODEX_COMPUTER");
        if let Some(explicit) = explicit {
            command.env("NANOCODEX_COMPUTER", explicit);
        }
        assert!(command.status().unwrap().success());
    };
    check(None, "absent".as_ref());
    fs::write(
        &receipt,
        serde_json::to_vec(&serde_json::json!({
            "status": "installed", "transport": "mcp", "executable": launcher
        }))
        .unwrap(),
    )
    .unwrap();
    // A selected missing launcher remains discoverable to surface startup errors.
    check(None, launcher.as_os_str());
    check(Some(""), launcher.as_os_str());
    check(Some("/explicit/provider"), "/explicit/provider".as_ref());
    for disabled in ["off", "none", "0"] {
        check(Some(disabled), "absent".as_ref());
    }
    for invalid in [
        "invalid json".to_owned(),
        serde_json::json!({"status":"installed", "transport":"mcp", "executable":"relative"})
            .to_string(),
        " ".repeat(65537),
    ] {
        fs::write(&receipt, invalid).unwrap();
        check(None, "absent".as_ref());
    }
    fs::remove_dir_all(root).unwrap();
}
