use super::*;

#[test]
fn archive_urls_are_restricted_to_the_official_versioned_feed() {
    assert!(validate_archive_url("https://persistent.oaistatic.com/codex-app-prod/ChatGPT-darwin-arm64-26.917.61114.zip").is_ok());
    assert!(validate_archive_url("https://example.com/codex-app-prod/ChatGPT-darwin-arm64-x.zip").is_err());
    assert!(validate_archive_url("https://persistent.oaistatic.com/codex-app-prod/appcast.xml").is_err());
    assert!(validate_archive_url("https://persistent.oaistatic.com/codex-app-prod/ChatGPT-darwin-arm64-x.zip?changed=1").is_err());
}

#[test]
fn component_selection_excludes_the_desktop_shell() {
    let prefix = "ChatGPT.app/";
    for name in [
        "ChatGPT.app/Contents/Info.plist",
        "ChatGPT.app/Contents/MacOS/ChatGPT",
        "ChatGPT.app/Contents/_CodeSignature/CodeResources",
        "ChatGPT.app/Contents/Resources/codex",
        "ChatGPT.app/Contents/Resources/cua_node/bin/node",
        "ChatGPT.app/Contents/Resources/plugins/openai-bundled/plugins/chrome/scripts/installManifest.mjs",
    ] { assert!(selected_name(name, prefix), "{name}"); }
    for name in [
        "ChatGPT.app/Contents/Resources/app.asar",
        "ChatGPT.app/Contents/Frameworks/Electron Framework.framework/Electron Framework",
        "ChatGPT.app/Contents/Resources/locales/en.lproj",
    ] { assert!(!selected_name(name, prefix), "{name}"); }
}

#[test]
fn exact_content_range_is_required() {
    assert_eq!(content_range(b"HTTP/2 206\r\ncontent-range: bytes 10-19/123\r\n\r\n", 10, 19).unwrap(), 123);
    assert!(content_range(b"HTTP/2 200\r\ncontent-length: 10\r\n", 10, 19).is_err());
    assert!(content_range(b"HTTP/2 206\r\ncontent-range: bytes 0-9/123\r\n", 10, 19).is_err());
}

#[test]
fn parses_bounded_classic_zip_directory() {
    let mut central = vec![0u8; 46];
    central[0..4].copy_from_slice(b"PK\x01\x02");
    central[28..30].copy_from_slice(&8u16.to_le_bytes());
    central[42..46].copy_from_slice(&7u32.to_le_bytes());
    central.extend_from_slice(b"file.txt");
    let entries = zip_entries(&central, 1).unwrap();
    assert_eq!(entries[0].name, "file.txt");
    assert_eq!(entries[0].local, 7);
    assert!(zip_entries(&central, 2).is_err());
}

#[test]
fn launcher_uses_only_headless_upstream_components() {
    let script = launcher(Path::new("/tmp/runtime")).unwrap();
    assert!(script.contains("Contents/Resources/codex"));
    assert!(script.contains("cua_node/bin/node"));
    assert!(script.contains("BROWSER_USE_TINYSKY_ENABLED=1"));
    assert!(!script.contains("Contents/MacOS"));
    assert!(HOST_MODULES.iter().all(|(name, _)| !name.contains("gui-readiness")));
}

#[test]
fn node_path_delimiters_are_rejected() {
    assert!(launcher(Path::new("/tmp/invalid:modules")).is_err());
    assert!(launcher(Path::new("/tmp/new\nline")).is_err());
}

#[test]
fn removes_only_the_unsealed_legacy_browser_config() {
    let directory = Staging {
        path: std::env::temp_dir().join(format!("nanocodex-browser-config-test-{}", nonce())),
        cleanup: true,
    };
    let app = directory.path.join("Codex.app");
    let relative = legacy_browser_config_relative();
    let config = app.join("Contents").join(&relative);
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(&config, b"{\"generated\":true}").unwrap();

    remove_unsealed_legacy_browser_config(&app, &HashMap::new()).unwrap();
    assert!(!config.exists());

    fs::write(&config, b"{\"upstream\":true}").unwrap();
    let mut seals = HashMap::new();
    seals.insert(
        relative.to_string_lossy().replace('\\', "/"),
        Seal::Hash([0; 32]),
    );
    remove_unsealed_legacy_browser_config(&app, &seals).unwrap();
    assert!(config.is_file());
}

#[test]
fn browser_bridge_uses_an_immutable_host_copy() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = Staging {
        path: std::env::temp_dir().join(format!("nanocodex-browser-host-test-{}", nonce())),
        cleanup: true,
    };
    let root = directory.path.join("runtime");
    let version = directory.path.join("version");
    for (source, relative) in browser_assets(&version) {
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, relative.to_string_lossy().as_bytes()).unwrap();
        if relative == PathBuf::from("browser").join(browser_extension_host()) {
            fs::set_permissions(&source, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    let host = ensure_host(&root, &version, &[("host.mjs", "export default true;")]).unwrap();
    for (source, relative) in browser_assets(&version) {
        assert_eq!(fs::read(source).unwrap(), fs::read(host.join(relative)).unwrap());
    }
    assert!(!version
        .join(APP)
        .join("Contents")
        .join(legacy_browser_config_relative())
        .exists());

    let copied_installer = host.join("browser/scripts/installManifest.mjs");
    fs::write(&copied_installer, b"modified").unwrap();
    let error = ensure_host(&root, &version, &[("host.mjs", "export default true;")])
        .unwrap_err();
    assert!(error.contains("managed browser host asset is modified"), "{error}");
}

#[test]
fn system_drains_both_full_pipes_and_reports_exit_status() {
    let cancellation = Cancellation::new();
    let mut system = System::new(cancellation.flag());
    let text = system
        .run(
            "/bin/sh",
            &[
                "-c".into(),
                "i=0; while [ \"$i\" -lt 8192 ]; do printf 'out-line-0123456789\\n'; printf 'err-line-0123456789\\n' >&2; i=$((i + 1)); done".into(),
            ],
        )
        .unwrap();
    assert_eq!(
        text,
        format!(
            "{}{}",
            "out-line-0123456789\n".repeat(8192),
            "err-line-0123456789\n".repeat(8192)
        )
    );
    let error = system
        .run(
            "/bin/sh",
            &["-c".into(), "printf expected-failure >&2; exit 7".into()],
        )
        .unwrap_err();
    assert!(
        error.contains('7') && error.contains("expected-failure"),
        "{error}"
    );
}

#[tokio::test]
async fn dropping_setup_cancels_the_owned_process_group() {
    let directory = Staging {
        path: std::env::temp_dir().join(format!("nanocodex-cancellation-test-{}", nonce())),
        cleanup: true,
    };
    fs::create_dir(&directory.path).unwrap();
    let pid_file = directory.path.join("owned-pids");
    let child_file = pid_file.clone();
    let (completed, result) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let cancellation = Cancellation::new();
        let mut system = System::new(cancellation.flag());
        tokio::task::spawn_blocking(move || {
            let result = system.run(
                "/bin/sh",
                &[
                    "-c".into(),
                    "sleep 60 & descendant=$!; printf '%s %s\\n' \"$$\" \"$descendant\" > \"$1\"; wait \"$descendant\"".into(),
                    "owned-cancel-test".into(),
                    child_file.into_os_string(),
                ],
            );
            let _ = completed.send(result);
        })
        .await
        .unwrap();
        drop(cancellation);
    });
    let pids = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Ok(text) = fs::read_to_string(&pid_file) {
                let pids: Vec<libc::pid_t> = text
                    .split_whitespace()
                    .map(|pid| pid.parse().unwrap())
                    .collect();
                if pids.len() == 2 {
                    break pids;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(unsafe { libc::getpgid(pids[0]) }, pids[0]);
    assert_eq!(unsafe { libc::getpgid(pids[1]) }, pids[0]);
    assert_ne!(unsafe { libc::getpgrp() }, pids[0]);

    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let error = tokio::time::timeout(std::time::Duration::from_secs(1), result)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error, CANCELLED);
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(pids[0], &mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        if unsafe { libc::kill(pids[1], 0) } == -1 {
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
            break;
        }
        #[cfg(target_os = "linux")]
        if fs::read_to_string(format!("/proc/{}/stat", pids[1])).is_ok_and(|stat| {
            stat.rsplit_once(") ")
                .is_some_and(|(_, tail)| tail.starts_with("Z "))
        }) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "descendant {} survived cancellation",
            pids[1]
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
