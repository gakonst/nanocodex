use super::*;

#[test]
fn archive_urls_are_restricted_to_the_official_versioned_feed() {
    assert!(
        validate_archive_url(
            "https://persistent.oaistatic.com/codex-app-prod/ChatGPT-darwin-arm64-26.917.61114.zip"
        )
        .is_ok()
    );
    assert!(
        validate_archive_url("https://example.com/codex-app-prod/ChatGPT-darwin-arm64-x.zip")
            .is_err()
    );
    assert!(
        validate_archive_url("https://persistent.oaistatic.com/codex-app-prod/appcast.xml")
            .is_err()
    );
    assert!(
        validate_archive_url(
            "https://persistent.oaistatic.com/codex-app-prod/ChatGPT-darwin-arm64-x.zip?changed=1"
        )
        .is_err()
    );
}

#[test]
fn component_selection_excludes_the_desktop_shell_and_chrome_proxy() {
    let prefix = "ChatGPT.app/";
    assert!(!selected_name(
        "Codex.app/Contents/Resources/codex",
        "Codex.app/"
    ));
    assert!(selected_name(
        "Codex.app/Contents/MacOS/Codex",
        "Codex.app/"
    ));
    for name in [
        "ChatGPT.app/Contents/Info.plist",
        "ChatGPT.app/Contents/MacOS/ChatGPT",
        "ChatGPT.app/Contents/_CodeSignature/CodeResources",
        "ChatGPT.app/Contents/Resources/cua_node/bin/node",
    ] {
        assert!(selected_name(name, prefix), "{name}");
    }
    for name in [
        "ChatGPT.app/Contents/Resources/plugins/openai-bundled/plugins/chrome/scripts/installManifest.mjs",
        "ChatGPT.app/Contents/Resources/plugins/openai-bundled/plugins/chrome/extension-host/macos/arm64/ChatGPT for Chrome",
        "ChatGPT.app/Contents/Resources/plugins/openai-bundled/plugins/chrome/extension-host/macos/x64/ChatGPT for Chrome",
        "ChatGPT.app/Contents/Resources/codex",
        "ChatGPT.app/Contents/Resources/app.asar",
        "ChatGPT.app/Contents/Frameworks/Electron Framework.framework/Electron Framework",
        "ChatGPT.app/Contents/Resources/locales/en.lproj",
    ] {
        assert!(!selected_name(name, prefix), "{name}");
    }
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
fn node_path_delimiters_are_rejected() {
    assert!(launcher(Path::new("/tmp/invalid:modules"), Path::new("/tmp/host")).is_err());
    assert!(launcher(Path::new("/tmp/new\nline"), Path::new("/tmp/host")).is_err());
}

#[test]
fn host_generations_are_immutable_and_independent_of_official_chrome_assets() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = test_directory("native-host");
    let root = directory.path.join("runtime");
    let version = directory.path.join("version");
    // Host construction does not require or read any Chrome plugin assets.
    let host = ensure_host(&root, &version, &[("host.mjs", "export default true;")]).unwrap();
    assert!(!host.join("browser").exists());
    assert_eq!(fs::read_dir(&host).unwrap().count(), 6);
    let upstream = fs::read_to_string(host.join("upstream-cua-provider")).unwrap();
    assert!(upstream.contains("CUA_REPL_ENABLED_SURFACES=browser,computer\n"));
    assert!(!upstream.contains("installManifest"));
    assert!(!upstream.contains("ChatGPT for Chrome"));
    assert!(!version.exists());
    assert!(
        fs::symlink_metadata(host.join("cua-policy-host"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111
            != 0
    );
    // Adding unrelated Chrome files does not affect the generated host hash.
    let plugin = version.join(APP).join(RESOURCES).join(BROWSER_PLUGIN);
    fs::create_dir_all(&plugin).unwrap();
    fs::write(plugin.join("unselected"), b"browser proxy").unwrap();
    assert_eq!(
        host,
        ensure_host(&root, &version, &[("host.mjs", "export default true;")]).unwrap()
    );
    let updated = ensure_host(&root, &version, &[("host.mjs", "export default false;")]).unwrap();
    assert_ne!(host, updated);
    assert_eq!(
        fs::read(host.join("host.mjs")).unwrap(),
        b"export default true;"
    );
    assert_eq!(
        fs::read(updated.join("host.mjs")).unwrap(),
        b"export default false;"
    );
    assert!(!updated.join("browser").exists());

    fs::write(host.join("cua-policy-host"), b"modified").unwrap();
    let error = ensure_host(&root, &version, &[("host.mjs", "export default true;")]).unwrap_err();
    assert!(error.contains("managed host asset is modified"), "{error}");
}

#[derive(Default)]
struct RecordingCommands {
    calls: Vec<(String, Vec<OsString>)>,
    bad_identity: bool,
}
impl Commands for RecordingCommands {
    fn run(&mut self, program: &str, args: &[OsString]) -> Result<String, String> {
        self.calls.push((program.to_owned(), args.to_vec()));
        if program.ends_with("codesign") && args.iter().any(|arg| arg == "--display") {
            if self.bad_identity {
                return Ok(format!("TeamIdentifier=NOT_OPENAI\nIdentifier={BUNDLE}\n"));
            }
            return Ok(format!("TeamIdentifier={TEAM}\nIdentifier={BUNDLE}\n"));
        }
        if program.ends_with("PlistBuddy") {
            return Ok(
                if args.iter().any(|arg| arg == "Print :CFBundleIdentifier") {
                    BUNDLE.to_owned()
                } else {
                    "9922".to_owned()
                },
            );
        }
        Ok(String::new())
    }
}

fn test_directory(label: &str) -> Staging {
    let directory = Staging {
        path: std::env::temp_dir().join(format!("nanocodex-{label}-{}", nonce())),
        cleanup: true,
    };
    fs::create_dir_all(&directory.path).unwrap();
    directory
}

fn lean_fixture(app: &Path) {
    let contents = app.join("Contents");
    let files = [
        "Resources/cua_node/bin/node".to_owned(),
        "Resources/cua_node/bin/node_repl".into(),
        format!("Resources/{MODULES}/{ENTRY}"),
        format!("Resources/{MODULES}/@oai/sky/package.json"),
        format!("Resources/{MODULES}/{SKY}/Contents/MacOS/SkyComputerUseService"),
    ];
    let mut manifest = String::from("<plist><dict><key>files2</key><dict>");
    for relative in files {
        let path = contents.join(&relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"original signed bytes").unwrap();
        let seal = base64::engine::general_purpose::STANDARD
            .encode(Sha256::digest(b"original signed bytes"));
        manifest.push_str(&format!(
            "<key>{relative}</key><dict><key>hash2</key><data>{seal}</data></dict>"
        ));
    }
    manifest.push_str("</dict></dict></plist>");
    fs::create_dir_all(contents.join("_CodeSignature")).unwrap();
    fs::write(contents.join("_CodeSignature/CodeResources"), manifest).unwrap();
    fs::write(contents.join("Info.plist"), b"original metadata").unwrap();
}

#[test]
fn lean_verification_preserves_signed_component_checks_without_codex() {
    let directory = test_directory("lean-verification");
    let app = directory.path.join(APP);
    lean_fixture(&app);
    let mut commands = RecordingCommands::default();
    assert_eq!(verify(&app, &mut commands).unwrap(), "9922");
    assert!(
        commands
            .calls
            .iter()
            .any(|(_, args)| args.iter().any(|arg| arg == "--ignore-resources"))
    );
    assert!(
        commands
            .calls
            .iter()
            .any(|(_, args)| args.iter().any(|arg| arg == TEAM_REQUIREMENT))
    );
    assert!(!commands.calls.iter().any(|(_, args)| {
        args.iter()
            .any(|arg| arg.to_string_lossy().ends_with("Resources/codex"))
    }));
    let mut wrong_signer = RecordingCommands {
        bad_identity: true,
        ..Default::default()
    };
    assert!(
        verify(&app, &mut wrong_signer)
            .unwrap_err()
            .contains("not the signed OpenAI")
    );
    let node = app.join(RESOURCES).join("cua_node/bin/node");
    fs::write(&node, b"tampered").unwrap();
    assert!(
        verify(&app, &mut commands)
            .unwrap_err()
            .contains("signed SHA-256 seal")
    );
    fs::remove_file(node).unwrap();
    assert!(
        verify(&app, &mut commands)
            .unwrap_err()
            .contains("incomplete")
    );
}

#[test]
fn old_selected_generation_requires_refresh_without_mutation() {
    let directory = test_directory("immutable-migration");
    let root = directory.path.join("runtimes/openai-cua");
    let app = root.join("versions/old").join(APP);
    fs::create_dir_all(app.join(RESOURCES)).unwrap();
    let codex = app.join(RESOURCES).join("codex");
    fs::write(&codex, b"old running CLI").unwrap();
    std::os::unix::fs::symlink("versions/old", root.join("current")).unwrap();
    fs::write(root.join("provider.json"), b"old complete receipt").unwrap();
    let config = app.join("Contents").join(legacy_browser_config_relative());
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(&config, b"old live browser config").unwrap();
    for refresh in [false, true] {
        let mut commands = RecordingCommands::default();
        assert!(
            cached(
                &root,
                &mut commands,
                refresh,
                &directory.path.join("home"),
                &[]
            )
            .unwrap()
            .is_none()
        );
        assert!(commands.calls.is_empty());
        assert_eq!(fs::read(&codex).unwrap(), b"old running CLI");
        assert_eq!(fs::read(&config).unwrap(), b"old live browser config");
        assert_eq!(
            fs::read(root.join("provider.json")).unwrap(),
            b"old complete receipt"
        );
        assert_eq!(
            fs::read_link(root.join("current")).unwrap(),
            Path::new("versions/old")
        );
    }
}

#[test]
fn warm_verification_cache_cannot_authorize_a_codex_bearing_layout() {
    let directory = test_directory("lean-cache");
    let app = directory.path.join(APP);
    lean_fixture(&app);
    let codex = app.join(RESOURCES).join("codex");
    fs::write(&codex, b"old CLI").unwrap();
    let fingerprint = crate::startup_cache::fingerprint(&app).unwrap();
    for format in [1, 2] {
        crate::startup_cache::write(
            &directory
                .path
                .join(format!(".startup-cache/verification-v{format}.json")),
            &VerificationRecord {
                format,
                verified_at: crate::startup_cache::now(),
                fingerprint: fingerprint.clone(),
                build: "9922".into(),
            },
        )
        .unwrap();
    }
    let mut commands = RecordingCommands::default();
    assert!(
        verified_cached(&directory.path, &app, &mut commands, false)
            .unwrap_err()
            .contains("fresh direct-CUA")
    );
    assert!(commands.calls.is_empty());
    fs::remove_file(&codex).unwrap();
    fs::create_dir(&codex).unwrap();
    assert!(verify_lean_layout(&app).is_err());
    fs::remove_dir(&codex).unwrap();
    std::os::unix::fs::symlink("missing", &codex).unwrap();
    assert!(verify_lean_layout(&app).is_err());
}

#[test]
fn browser_containing_cached_generation_is_rejected_without_mutation() {
    let directory = test_directory("browser-generation");
    let root = directory.path.join("runtime");
    let app = root.join("versions/old").join(APP);
    lean_fixture(&app);
    let plugin = app.join(RESOURCES).join(BROWSER_PLUGIN);
    fs::create_dir_all(&plugin).unwrap();
    let proxy = plugin.join("signed-proxy");
    fs::write(&proxy, b"old running app-server proxy").unwrap();
    std::os::unix::fs::symlink("versions/old", root.join("current")).unwrap();
    fs::write(root.join("provider.json"), b"old receipt").unwrap();
    let fingerprint = crate::startup_cache::fingerprint(&app).unwrap();
    crate::startup_cache::write(
        &root.join(".startup-cache/verification-v2.json"),
        &VerificationRecord {
            format: 2,
            verified_at: crate::startup_cache::now(),
            fingerprint,
            build: "9922".into(),
        },
    )
    .unwrap();
    for refresh in [false, true] {
        let mut commands = RecordingCommands::default();
        assert!(
            cached(
                &root,
                &mut commands,
                refresh,
                &directory.path.join("home"),
                &[]
            )
            .unwrap()
            .is_none()
        );
        assert!(commands.calls.is_empty());
        assert_eq!(fs::read(&proxy).unwrap(), b"old running app-server proxy");
        assert_eq!(
            fs::read(root.join("provider.json")).unwrap(),
            b"old receipt"
        );
        assert_eq!(
            fs::read_link(root.join("current")).unwrap(),
            Path::new("versions/old")
        );
        assert!(verified_cached(&root, &app, &mut commands, refresh).is_err());
    }
    fs::remove_dir_all(&plugin).unwrap();
    std::os::unix::fs::symlink("missing", &plugin).unwrap();
    assert!(requires_fresh_generation(&app).unwrap());
}

#[test]
fn legacy_browser_config_is_migrated_without_deleting_the_live_file() {
    let directory = test_directory("browser-config-migration");
    let app = directory.path.join(APP);
    lean_fixture(&app);
    let config = app.join("Contents").join(legacy_browser_config_relative());
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(&config, b"live unsealed config").unwrap();
    assert!(requires_fresh_generation(&app).unwrap());
    assert_eq!(fs::read(&config).unwrap(), b"live unsealed config");
}

#[test]
fn direct_host_wrappers_have_no_official_cli_or_app_server_target() {
    let version = Path::new("/private/runtime/versions/build");
    let root = Path::new("/private/runtime/runtimes/openai-cua");
    let host = Path::new("/private/runtime/hosts/HASH");
    let wrapper = host_launcher(root, version, host, "HASH").unwrap();
    for variable in [
        "NANOCODEX_CUA_NATIVE_APP",
        "NANOCODEX_CUA_NATIVE_PROVIDER",
        "NANOCODEX_CUA_POLICY_HOST",
        "NANOCODEX_CUA_NATIVE_STATE",
    ] {
        assert!(wrapper.contains(variable));
    }
    assert!(wrapper.contains("direct-cua-host.mjs"));
    assert!(wrapper.contains("/private/runtime/s"));
    assert!(wrapper.contains("${NANOCODEX_CUA_APP_CONSENT:-allow}"));
    assert!(!wrapper.contains("Resources/codex"));
    assert!(!wrapper.contains("app-server"));
    let policy = policy_launcher(version, host).unwrap();
    assert!(policy.contains("direct-cua-host.mjs' --policy \"$@\""));
    assert!(executable_host_asset("cua-policy-host"));
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

// This narrow integration boundary is necessary on Linux: provisioning itself
// requires Apple's codesign and ditto. Only the HTTPS origin is a fixture; ZIP
// selection, transfer, cancellation, assembly and curl are production code.
#[test]
#[ignore = "requires python3, openssl and /usr/bin/curl; runs a local HTTPS journey"]
fn production_parallel_downloader_https_journey() {
    use std::{io::BufRead as _, os::unix::process::CommandExt as _, time::Instant};

    let directory = test_directory("parallel-download");
    let script = directory.path.join("server.py");
    fs::write(
        &script,
        include_str!("../tests/fixtures/cua_archive_server.py"),
    )
    .unwrap();
    let mut server = OwnedCommand::new(
        std::process::Command::new("python3")
            .args([script.as_os_str(), directory.path.as_os_str()])
            .stdout(std::process::Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap(),
    );
    let mut line = String::new();
    std::io::BufReader::new(server.child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let endpoint: serde_json::Value = serde_json::from_str(&line).expect("HTTPS fixture startup");
    let url = endpoint["url"].as_str().unwrap();
    let length = endpoint["length"].as_u64().unwrap();
    struct HttpsCommands {
        system: System,
        certificate: PathBuf,
        serial: bool,
    }
    impl Commands for HttpsCommands {
        fn check_cancelled(&self) -> Result<(), String> {
            self.system.check_cancelled()
        }
        fn run(&mut self, program: &str, args: &[OsString]) -> Result<String, String> {
            assert_eq!(program, "/usr/bin/curl");
            let mut trusted = Vec::new();
            for (index, arg) in args.iter().enumerate() {
                if index > 0 && args[index - 1] == "--parallel-max" && self.serial {
                    trusted.push("1".into());
                } else {
                    trusted.push(arg.clone());
                }
                if arg == "--disable" || arg == "--next" {
                    trusted.extend([
                        "--cacert".into(),
                        self.certificate.as_os_str().to_owned(),
                        "--noproxy".into(),
                        "*".into(),
                    ]);
                }
            }
            let cancel = if args
                .iter()
                .any(|arg| arg.to_string_lossy().ends_with("/cancelled"))
                && args.iter().filter(|arg| *arg == "--range").count() > 1
            {
                let flag = self.system.cancelled.clone();
                Some(std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    flag.store(true, Ordering::Release);
                }))
            } else {
                None
            };
            let result = self.system.run(program, &trusted);
            if let Some(cancel) = cancel {
                cancel.join().unwrap();
            }
            result
        }
    }
    let cancellation = Cancellation::new();
    let mut commands = HttpsCommands {
        system: System::new(cancellation.flag()),
        certificate: directory.path.join("cert.pem"),
        serial: true,
    };
    let mut timings = Vec::new();
    for mode in [
        "serial",
        "parallel",
        "unknown-length",
        "wrongrange",
        "missingrange",
        "changed",
        "short",
        "oversize",
        "failure",
        "recovery",
        "cancelled",
    ] {
        commands.serial = mode == "serial";
        let stage = directory.path.join(mode);
        fs::create_dir(&stage).unwrap();
        let begin = Instant::now();
        let release = Release {
            build: "fixture".into(),
            url: format!("{url}/{mode}"),
            length: if mode == "unknown-length" { 0 } else { length },
        };
        let result = component_zip(&stage, &mut commands, &release);
        timings.push((mode, begin.elapsed().as_millis()));
        if matches!(mode, "serial" | "parallel" | "unknown-length" | "recovery") {
            let archive = result.unwrap();
            let verified = std::process::Command::new("python3")
                .args([
                    script.as_os_str(),
                    directory.path.as_os_str(),
                    archive.as_os_str(),
                ])
                .output()
                .unwrap();
            assert!(
                verified.status.success(),
                "{}",
                String::from_utf8_lossy(&verified.stderr)
            );
            eprintln!(
                "{mode}: {}",
                String::from_utf8_lossy(&verified.stdout).trim()
            );
        } else {
            let error = result.unwrap_err();
            eprintln!("{mode}: rejected: {error}");
            if mode == "cancelled" {
                assert_eq!(error, CANCELLED);
            }
            assert!(
                !stage.join("components.zip").exists(),
                "failed download published an archive"
            );
        }
    }
    let trace = fs::read_to_string(directory.path.join("requests.jsonl")).unwrap();
    let requests: Vec<serde_json::Value> = trace
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let peak = requests
        .iter()
        .filter(|r| r["path"] == "/parallel")
        .map(|r| r["active"].as_u64().unwrap())
        .max()
        .unwrap();
    assert!(
        (4..=RANGE_PARALLELISM as u64).contains(&peak),
        "observed peak {peak}"
    );
    let serial_peak = requests
        .iter()
        .filter(|r| r["path"] == "/serial")
        .map(|r| r["active"].as_u64().unwrap())
        .max()
        .unwrap();
    assert_eq!(serial_peak, 1);
    // Known-length feeds avoid the extra probe, and this ZIP's central directory
    // fits in the tail. Exactly one metadata request precedes payload transfers.
    let parallel: Vec<_> = requests
        .iter()
        .filter(|r| r["path"] == "/parallel")
        .collect();
    assert_eq!(
        parallel
            .iter()
            .filter(|r| r["end"].as_u64().unwrap() - r["start"].as_u64().unwrap() < 65557)
            .count(),
        1
    );
    assert!(
        parallel
            .iter()
            .all(|r| r["end"].as_u64().unwrap() - r["start"].as_u64().unwrap() < RANGE_CHUNK_BYTES)
    );
    eprintln!(
        "actual curl HTTPS journey: peak_parallel={peak}; timings_ms={timings:?}; trace={trace}"
    );
}

/// Opt-in real upstream journey, isolated from HOME and runtime publication.
/// On macOS this also runs ditto, all OpenAI signature checks and signed seals.
#[test]
#[ignore = "downloads official components; set CUA_JOURNEY_URL, CUA_JOURNEY_LENGTH, CUA_JOURNEY_BUILD"]
fn official_component_download_journey() {
    let mut directory = test_directory("official-download");
    fs::create_dir(directory.path.join("payload")).unwrap();
    let release = Release {
        url: std::env::var("CUA_JOURNEY_URL").unwrap(),
        length: std::env::var("CUA_JOURNEY_LENGTH")
            .unwrap()
            .parse()
            .unwrap(),
        build: std::env::var("CUA_JOURNEY_BUILD").unwrap(),
    };
    validate_archive_url(&release.url).unwrap();
    let cancellation = Cancellation::new();
    let mut system = System::new(cancellation.flag());
    let began = std::time::Instant::now();
    #[cfg(target_os = "macos")]
    {
        let app = download(&mut directory, &mut system, &release).unwrap();
        assert_eq!(verify(&app, &mut system).unwrap(), release.build);
        eprintln!(
            "official macOS signatures and SHA-256 seals verified for build {}",
            release.build
        );
        // A modified selected resource must still be refused after the optimized
        // transport. This stage can never be selected by the user's runtime.
        let resource = app.join(RESOURCES).join(MODULES).join(ENTRY);
        fs::write(resource, b"tampered integration fixture").unwrap();
        let error = verify(&app, &mut system).unwrap_err();
        assert!(error.contains("signed SHA-256 seal"), "{error}");
        eprintln!("tampered official component rejected: {error}");
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = &mut directory;
        let archive = component_zip(&directory.path, &mut system, &release).unwrap();
        let result = std::process::Command::new("python3")
            .args(["-c", "import sys,zipfile; z=zipfile.ZipFile(sys.argv[1]); assert z.testzip() is None; print('ZIP CRC verified:',len(z.namelist()),'entries')"])
            .arg(archive).output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        eprintln!("{}", String::from_utf8_lossy(&result.stdout).trim());
    }
    let archive = directory.path.join("components.zip");
    eprintln!(
        "official url={} full_bytes={} component_bytes={} elapsed_ms={}",
        release.url,
        release.length,
        fs::metadata(archive).unwrap().len(),
        began.elapsed().as_millis()
    );
}

// Attestation commands are the only fixture boundary: the complete cached setup
// publishes real immutable assets, manifests and receipts under an isolated home.
#[test]
fn full_setup_registers_browser_bridge_and_preserves_conflicts() {
    let directory = test_directory("browser-registration");
    let root = directory.path.join("runtime");
    let home = directory.path.join("home");
    let applications = directory.path.join("Applications");
    let version = root.join("versions/fixture");
    lean_fixture(&version.join(APP));
    fs::create_dir_all(applications.join("Google Chrome.app")).unwrap();
    fs::create_dir_all(applications.join("Brave Browser.app")).unwrap();
    std::os::unix::fs::symlink("versions/fixture", root.join("current")).unwrap();
    let browsers = [applications];
    let mut commands = RecordingCommands::default();
    let receipt = provision(&root, &home, &browsers, false, &mut commands).unwrap();
    assert_eq!(receipt["dependency_contract"], "nanocodex-direct-cua-v2");
    assert_eq!(receipt["environment"], serde_json::json!({}));
    let paths = receipt["browser_bridge"]["manifests"].as_array().unwrap();
    assert_eq!(paths.len(), 2);
    let host = Path::new(receipt["executable"].as_str().unwrap())
        .parent()
        .unwrap();
    for path in paths {
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(path.as_str().unwrap()).unwrap()).unwrap();
        assert_eq!(manifest, browser_manifest(host));
        assert!(Path::new(manifest["path"].as_str().unwrap()).is_file());
    }
    assert!(
        !home
            .join("Library/Application Support/Microsoft Edge")
            .exists()
    );
    assert_eq!(
        provision(&root, &home, &browsers, false, &mut commands).unwrap(),
        receipt
    );
    let previous_receipt = fs::read(root.join("provider.json")).unwrap();
    let first_manifest = PathBuf::from(paths[0].as_str().unwrap());
    let conflicting_manifest = PathBuf::from(paths[1].as_str().unwrap());
    // Even if the first browser needs repair, preflight protects it when another
    // browser belongs to the official Codex installation.
    fs::remove_file(&first_manifest).unwrap();
    let external =
        br#"{"name":"com.openai.codexextension","path":"/Applications/Codex.app/official-host"}"#;
    fs::write(&conflicting_manifest, external).unwrap();
    let error = provision(&root, &home, &browsers, false, &mut commands).unwrap_err();
    assert!(error.contains("manifest conflict"), "{error}");
    assert!(!first_manifest.exists());
    assert_eq!(fs::read(&conflicting_manifest).unwrap(), external);
    assert_eq!(
        fs::read(root.join("provider.json")).unwrap(),
        previous_receipt
    );
    assert_eq!(
        fs::read_link(root.join("current")).unwrap(),
        Path::new("versions/fixture")
    );
    fs::remove_file(&conflicting_manifest).unwrap();
    let recovered = provision(&root, &home, &browsers, false, &mut commands).unwrap();
    assert_eq!(recovered, receipt);
    eprintln!(
        "fixture setup: installed 2 browser manifests; cached rerun unchanged; official registration conflict preserved receipt and all other browser paths; recovery succeeded"
    );
}

#[test]
fn generated_launchers_execute_standalone_kernel_and_own_browser_host() {
    use std::os::unix::fs::PermissionsExt as _;
    let directory = test_directory("launch-contract");
    let root = directory.path.join("runtime");
    let version = directory.path.join("version with spaces");
    let bin = version.join(APP).join(RESOURCES).join("cua_node/bin");
    fs::create_dir_all(&bin).unwrap();
    // Probe executables observe real shell argv/environment without launching any
    // upstream process, browser, app server or live native messaging registration.
    for binary in ["node", "node_repl"] {
        let file = bin.join(binary);
        fs::write(&file, b"#!/bin/sh\nprintf '%s\\n' \"$@\"\nprintf 'surfaces=%s\\ntinysky=%s\\nambient=%s\\npolicy=%s\\n' \"${CUA_REPL_ENABLED_SURFACES-}\" \"${BROWSER_USE_TINYSKY_ENABLED-}\" \"${BROWSER_USE_DISABLE_AMBIENT_NETWORK-}\" \"${CODEX_CLI_PATH-}\"\nif [ -n \"${CUA_REPL_NODE_REPL_PATH-}\" ]; then printf 'kernel=%s\\n' \"$CUA_REPL_NODE_REPL_PATH\"; fi\n").unwrap();
        fs::set_permissions(file, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let host = ensure_host(&root, &version, HOST_MODULES).unwrap();
    let invoke = |name: &str, arg: &str| {
        let output = std::process::Command::new(host.join(name))
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("CODEX_CLI_PATH", "/own/policy-only-host")
            .arg(arg)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let upstream = invoke("upstream-cua-provider", "argument with spaces");
    assert!(upstream.contains(
        "surfaces=browser,computer\ntinysky=1\nambient=1\npolicy=/own/policy-only-host\n"
    ));
    assert!(upstream.contains(&format!("kernel={}\n", host.join("node-repl").display())));
    let kernel = invoke("node-repl", "kernel argument");
    assert!(
        kernel.starts_with("--disable-sandbox\nkernel argument\n"),
        "{kernel}"
    );
    let browser = invoke("native-browser-host", BROWSER_ORIGINS[0]);
    assert!(
        browser.starts_with(&format!(
            "{}\n{}\n",
            host.join("direct-browser-host.mjs").display(),
            BROWSER_ORIGINS[0]
        )),
        "{browser}"
    );
    assert!(browser.contains("policy=\n"), "{browser}");
    eprintln!(
        "fixture launcher argv: kernel=--disable-sandbox; provider=browser,computer + TinySky; native messaging=own bundled JS; ambient CLI removed from browser relay"
    );
}
