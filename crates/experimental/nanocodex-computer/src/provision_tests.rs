use super::*;
use std::process::Command;

struct Fixture {
    directory: PathBuf,
    fail_download: bool,
    bad_copy: bool,
    build: String,
    calls: Vec<String>,
}
impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!("nanocodex-upstream-test-{}", nonce()));
        fs::create_dir_all(&directory).unwrap();
        Self {
            directory,
            fail_download: false,
            bad_copy: false,
            build: "9922".into(),
            calls: Vec::new(),
        }
    }
    fn app(path: &Path) {
        for relative in [
            "Contents/Info.plist".to_owned(),
            format!("{RESOURCES}/codex"),
            format!("{RESOURCES}/cua_node/bin/node"),
            format!("{RESOURCES}/cua_node/bin/node_repl"),
            format!("{RESOURCES}/{MODULES}/{ENTRY}"),
            format!("{RESOURCES}/{MODULES}/@oai/sky/package.json"),
            format!("{RESOURCES}/{MODULES}/@oai/browser-desktop/package.json"),
            format!("{RESOURCES}/{MODULES}/{SKY}/Contents/MacOS/SkyComputerUseService"),
        ] {
            let file = path.join(relative);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, "signed fixture").unwrap();
        }
    }
    fn copy(source: &Path, target: &Path) {
        fs::create_dir_all(target).unwrap();
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let target = target.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                Self::copy(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).unwrap();
            }
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}
impl Commands for Fixture {
    fn run(&mut self, program: &str, args: &[OsString]) -> Result<String, String> {
        self.calls.push(program.to_owned());
        match program {
            "/usr/bin/codesign" => {
                let app = PathBuf::from(args.last().unwrap());
                if app.join("tampered").exists() {
                    return Err("invalid signature".into());
                }
                if args[0] == "--verify" {
                    assert!(args.iter().any(|a| a == "--deep"));
                    assert!(args.iter().any(|a| a == REQUIREMENT));
                    Ok(String::new())
                } else {
                    Ok(format!("TeamIdentifier={TEAM}\nIdentifier={BUNDLE}\n"))
                }
            }
            "/usr/libexec/PlistBuddy" => Ok(if args[1] == "Print :CFBundleIdentifier" {
                BUNDLE
            } else {
                &self.build
            }
            .into()),
            "/usr/sbin/sysctl" => Ok("1".into()),
            "/usr/bin/curl" => {
                assert_eq!(args.last().unwrap(), ARM_DMG);
                assert!(args.iter().any(|a| a == "--proto-redir"));
                if self.fail_download {
                    Err("network unavailable".into())
                } else {
                    Ok(String::new())
                }
            }
            "/usr/bin/hdiutil" => {
                if args[0] == "attach" {
                    assert!(args.iter().any(|a| a == "-readonly"));
                    let i = args.iter().position(|a| a == "-mountpoint").unwrap();
                    Self::app(&PathBuf::from(&args[i + 1]).join("ChatGPT.app"));
                }
                Ok(String::new())
            }
            "/usr/bin/ditto" => {
                Self::copy(Path::new(&args[0]), Path::new(&args[1]));
                if self.bad_copy {
                    fs::write(PathBuf::from(&args[1]).join("tampered"), "changed").unwrap();
                }
                Ok(String::new())
            }
            _ => panic!("unexpected program {program}"),
        }
    }
}

#[test]
fn installed_app_is_copied_verified_and_reused_without_network() {
    let mut f = Fixture::new();
    let applications = f.directory.join("Applications");
    Fixture::app(&applications.join("ChatGPT.app"));
    let root = f.directory.join("install ' literal/runtimes/openai-cua");
    let receipt = provision(&root, &[applications], false, &mut f).unwrap();
    assert_eq!(receipt["transport"], "mcp");
    let provider = PathBuf::from(receipt["executable"].as_str().unwrap());
    assert!(provider.starts_with(root.join("hosts")));
    assert_eq!(receipt["args"], serde_json::json!([]));
    assert_eq!(receipt["environment"], serde_json::json!({}));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(root.join("provider.json")).unwrap())
            .unwrap(),
        receipt
    );
    assert!(!f.calls.iter().any(|s| s.ends_with("curl")));
    let script =
        fs::read_to_string(provider.parent().unwrap().join("upstream-cua-provider")).unwrap();
    assert!(script.contains("CODEX_CLI_PATH="));
    assert!(script.contains("NODE_REPL_UNTRUSTED_ENV_ALLOWLIST=SKY_CUA_SERVICE_PATH"));
    assert!(
        Command::new("/bin/sh")
            .args(["-n"])
            .arg(&provider)
            .status()
            .unwrap()
            .success()
    );
    f.calls.clear();
    assert_eq!(provision(&root, &[], false, &mut f).unwrap(), receipt);
    assert!(
        !f.calls
            .iter()
            .any(|s| s.ends_with("curl") || s.ends_with("ditto"))
    );
}

#[test]
fn launcher_enables_browser_ax_in_trusted_provider_environment() {
    use std::os::unix::fs::PermissionsExt;

    let f = Fixture::new();
    let version = f.directory.join("runtime ' quoted");
    Fixture::app(&version.join(APP));
    let node = version.join(APP).join(RESOURCES).join("cua_node/bin/node");
    fs::write(
        &node,
        "#!/bin/sh\nprintf '%s\\n' \"$BROWSER_USE_TINYSKY_ENABLED\" \"$CUA_REPL_ENABLED_SURFACES\" \"$NODE_REPL_UNTRUSTED_ENV_ALLOWLIST\"\n",
    )
    .unwrap();
    fs::set_permissions(&node, fs::Permissions::from_mode(0o755)).unwrap();
    let script = version.join("cua-provider");
    fs::write(&script, launcher(&version).unwrap()).unwrap();
    let output = Command::new("/bin/sh")
        .arg(script)
        .env("BROWSER_USE_TINYSKY_ENABLED", "0")
        .env_remove("NODE_REPL_UNTRUSTED_ENV_ALLOWLIST")
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "1\nbrowser,computer\nSKY_CUA_SERVICE_PATH\n"
    );
}

#[test]
fn clean_install_downloads_and_detaches_before_atomic_selection() {
    let mut f = Fixture::new();
    let root = f.directory.join("runtime");
    let receipt = provision(&root, &[], false, &mut f).unwrap();
    assert!(Path::new(receipt["executable"].as_str().unwrap()).is_file());
    assert!(!root.join("current/cua-provider").exists());
    assert_eq!(f.calls.iter().filter(|s| s.ends_with("hdiutil")).count(), 2);
    assert!(fs::read_dir(&root).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".staging")
    }));
}

#[test]
fn failed_refresh_preserves_previous_selection_and_success_keeps_old_version() {
    let mut f = Fixture::new();
    let root = f.directory.join("runtime");
    provision(&root, &[], false, &mut f).unwrap();
    let previous = fs::read_link(root.join("current")).unwrap();
    f.fail_download = true;
    assert!(provision(&root, &[], true, &mut f).is_err());
    assert_eq!(fs::read_link(root.join("current")).unwrap(), previous);
    f.fail_download = false;
    f.bad_copy = true;
    assert!(
        provision(&root, &[], true, &mut f)
            .unwrap_err()
            .contains("signature")
    );
    assert_eq!(fs::read_link(root.join("current")).unwrap(), previous);
    f.bad_copy = false;
    provision(&root, &[], true, &mut f).unwrap();
    assert_eq!(fs::read_link(root.join("current")).unwrap(), previous);
    assert!(root.join(previous).join(APP).is_dir());
}

#[test]
fn corrupted_cache_requires_explicit_repair_instead_of_another_backend() {
    let mut f = Fixture::new();
    let root = f.directory.join("runtime");
    let receipt = provision(&root, &[], false, &mut f).unwrap();
    fs::write(receipt["executable"].as_str().unwrap(), "modified").unwrap();
    f.calls.clear();
    assert!(
        provision(&root, &[], false, &mut f)
            .unwrap_err()
            .contains("--refresh")
    );
    assert!(!f.calls.iter().any(|s| s.ends_with("curl")));
    provision(&root, &[], true, &mut f).unwrap();
    fs::write(root.join("current/Codex.app/tampered"), "modified").unwrap();
    assert!(
        provision(&root, &[], false, &mut f)
            .unwrap_err()
            .contains("signature")
    );
}

#[test]
fn node_path_delimiters_are_rejected() {
    assert!(launcher(Path::new("/tmp/invalid:modules")).is_err());
    assert!(launcher(Path::new("/tmp/new\nline")).is_err());
}

#[test]
fn host_upgrade_preserves_signed_bundle_and_ignores_old_generated_launcher() {
    let mut f = Fixture::new();
    let root = f.directory.join("runtime");
    let receipt = provision(&root, &[], false, &mut f).unwrap();
    let selected = fs::read_link(root.join("current")).unwrap();
    let version = root.join(&selected);
    let signed_file = version.join(APP).join(RESOURCES).join("codex");
    let signed_bytes = fs::read(&signed_file).unwrap();
    // An older install may contain its generated launcher beside Codex.app.
    // Its bytes do not participate in signed-bundle reuse, nor is it executed.
    fs::write(version.join("cua-provider"), "#!/bin/sh\nexit 99\n").unwrap();
    assert_eq!(provision(&root, &[], false, &mut f).unwrap(), receipt);
    let mut next_modules = HOST_MODULES.to_vec();
    next_modules[0].1 = "// synthetic next host revision\n";
    let next = ensure_host(&root, &version, &next_modules).unwrap();
    assert_ne!(
        next.join("cua-provider"),
        PathBuf::from(receipt["executable"].as_str().unwrap())
    );
    assert_eq!(ensure_host(&root, &version, &next_modules).unwrap(), next);
    assert_eq!(fs::read_link(root.join("current")).unwrap(), selected);
    assert_eq!(fs::read(signed_file).unwrap(), signed_bytes);
    assert!(Path::new(receipt["executable"].as_str().unwrap()).is_file());
    fs::write(next.join(next_modules[0].0), "tampered").unwrap();
    assert!(
        ensure_host(&root, &version, &next_modules)
            .unwrap_err()
            .contains("modified")
    );
}

#[test]
fn host_wrapper_pins_trusted_paths_and_forwards_arguments() {
    use std::os::unix::fs::PermissionsExt;
    let mut f = Fixture::new();
    let root = f.directory.join("runtime ' quoted");
    let receipt = provision(&root, &[], false, &mut f).unwrap();
    let version = root.join(fs::read_link(root.join("current")).unwrap());
    let host = Path::new(receipt["executable"].as_str().unwrap())
        .parent()
        .unwrap();
    let node = version.join(APP).join(RESOURCES).join("cua_node/bin/node");
    fs::write(&node, "#!/bin/sh\nprintf '%s\\n' \"$NANOCODEX_CUA_NATIVE_APP\" \"$NANOCODEX_CUA_NATIVE_PROVIDER\" \"$NANOCODEX_CUA_NATIVE_STATE\" \"$@\"\n").unwrap();
    fs::set_permissions(&node, fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(host.join("cua-provider"))
        .arg("literal argument")
        .env("NANOCODEX_CUA_NATIVE_APP", "/untrusted")
        .env("NANOCODEX_CUA_NATIVE_PROVIDER", "/untrusted")
        .env("NANOCODEX_CUA_NATIVE_STATE", "/untrusted")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "{}\n{}\n{}\n{}\nliteral argument\n",
            version.join(APP).display(),
            host.join("upstream-cua-provider").display(),
            root.join("host-state")
                .join(host.file_name().unwrap())
                .display(),
            host.join("openai-cua-native-host.mjs").display()
        )
    );
}

#[test]
fn concurrent_host_publication_reuses_the_complete_winner() {
    let mut f = Fixture::new();
    let root = f.directory.join("runtime");
    provision(&root, &[], false, &mut f).unwrap();
    let version = root.join(fs::read_link(root.join("current")).unwrap());
    let mut modules = HOST_MODULES.to_vec();
    modules[0].1 = "// concurrent revision\n";
    std::thread::scope(|scope| {
        let threads: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| ensure_host(&root, &version, &modules).unwrap()))
            .collect();
        let paths: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert!(paths.iter().all(|path| path == &paths[0]));
    });
}

#[test]
fn cached_selection_and_assets_reject_symlink_substitution() {
    let mut f = Fixture::new();
    let root = f.directory.join("runtime");
    let receipt = provision(&root, &[], false, &mut f).unwrap();
    let provider = Path::new(receipt["executable"].as_str().unwrap());
    let copied = f.directory.join("identical-provider");
    fs::copy(provider, &copied).unwrap();
    fs::remove_file(provider).unwrap();
    std::os::unix::fs::symlink(copied, provider).unwrap();
    assert!(
        provision(&root, &[], false, &mut f)
            .unwrap_err()
            .contains("modified")
    );
    let selected = fs::read_link(root.join("current")).unwrap();
    fs::remove_file(root.join("current")).unwrap();
    std::os::unix::fs::symlink(root.join(selected), root.join("current")).unwrap();
    assert!(
        provision(&root, &[], false, &mut f)
            .unwrap_err()
            .contains("current must select a managed version")
    );
}

#[test]
fn supported_build_matches_embedded_readiness_contract() {
    let readiness = HOST_MODULES
        .iter()
        .find(|(name, _)| *name == "openai-cua-gui-readiness.mjs")
        .unwrap()
        .1;
    let declaration = readiness
        .lines()
        .find_map(|line| line.strip_prefix("export const KNOWN_GUI_BUILD = "))
        .unwrap();
    assert_eq!(declaration, format!("'{SUPPORTED_GUI_BUILD}';"));
    assert_eq!(Fixture::new().build, SUPPORTED_GUI_BUILD);
}

#[test]
fn unsupported_refresh_preserves_previous_bundle_host_and_receipt() {
    let mut f = Fixture::new();
    let root = f.directory.join("runtime");
    let receipt = provision(&root, &[], false, &mut f).unwrap();
    let selected = fs::read_link(root.join("current")).unwrap();
    let published = fs::read(root.join("provider.json")).unwrap();
    let provider = Path::new(receipt["executable"].as_str().unwrap());
    let host = fs::read(provider).unwrap();
    f.build = "1234".into();
    f.calls.clear();
    let error = provision(&root, &[], true, &mut f).unwrap_err();
    assert!(
        error.contains("Unsupported OpenAI CUA build 1234"),
        "{error}"
    );
    assert_eq!(fs::read_link(root.join("current")).unwrap(), selected);
    assert_eq!(fs::read(root.join("provider.json")).unwrap(), published);
    assert_eq!(fs::read(provider).unwrap(), host);
    assert_eq!(fs::read_dir(root.join("versions")).unwrap().count(), 1);
    assert_eq!(fs::read_dir(root.join("hosts")).unwrap().count(), 1);
    assert_eq!(f.calls.iter().filter(|s| s.ends_with("hdiutil")).count(), 2);
    assert!(!f.calls.iter().any(|s| s.ends_with("ditto")));
    assert!(fs::read_dir(&root).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".staging")
    }));
}

#[test]
fn failed_host_preparation_preserves_previous_selection_and_receipt() {
    let mut f = Fixture::new();
    let root = f.directory.join("runtime");
    let receipt = provision(&root, &[], false, &mut f).unwrap();
    let selected = fs::read_link(root.join("current")).unwrap();
    let published = fs::read(root.join("provider.json")).unwrap();
    // Block host preparation independently of bundle verification/publication.
    fs::rename(root.join("hosts"), root.join("saved-hosts")).unwrap();
    fs::write(root.join("hosts"), "blocked host directory").unwrap();
    assert!(provision(&root, &[], true, &mut f).is_err());
    assert_eq!(fs::read_link(root.join("current")).unwrap(), selected);
    assert_eq!(fs::read(root.join("provider.json")).unwrap(), published);
    fs::remove_file(root.join("hosts")).unwrap();
    fs::rename(root.join("saved-hosts"), root.join("hosts")).unwrap();
    assert!(Path::new(receipt["executable"].as_str().unwrap()).is_file());
}

#[test]
fn unsupported_cached_build_does_not_suggest_refreshing_damage() {
    let mut f = Fixture::new();
    let root = f.directory.join("runtime");
    provision(&root, &[], false, &mut f).unwrap();
    let published = fs::read(root.join("provider.json")).unwrap();
    f.build = "1234".into();
    let error = provision(&root, &[], false, &mut f).unwrap_err();
    assert!(error.starts_with("Unsupported OpenAI CUA build 1234"));
    assert!(!error.contains("damaged"));
    assert!(!error.contains("--refresh"));
    assert_eq!(fs::read(root.join("provider.json")).unwrap(), published);
}

#[test]
fn unchanged_bundle_reuses_deep_verification_until_expiry_and_does_not_republish_receipt() {
    let mut fixture = Fixture::new();
    let root = fixture.directory.join("runtime");
    let receipt = provision(&root, &[], false, &mut fixture).unwrap();
    // The first reuse establishes a before/after fingerprint around full verify.
    assert_eq!(provision(&root, &[], false, &mut fixture).unwrap(), receipt);
    let published = fs::metadata(root.join("provider.json")).unwrap().modified().unwrap();
    fixture.calls.clear();
    assert_eq!(provision(&root, &[], false, &mut fixture).unwrap(), receipt);
    assert!(fixture.calls.is_empty(), "unchanged warm reuse must run no subprocesses");
    assert_eq!(fs::metadata(root.join("provider.json")).unwrap().modified().unwrap(), published);
    let path = root.join(".startup-cache/verification-v1.json");
    let mut record: VerificationRecord = crate::startup_cache::read(&path).unwrap();
    record.verified_at = crate::startup_cache::now() - crate::startup_cache::MAX_AGE_SECS;
    crate::startup_cache::write(&path, &record).unwrap();
    provision(&root, &[], false, &mut fixture).unwrap();
    assert_eq!(fixture.calls.iter().filter(|program| program.ends_with("codesign")).count(), 2);
}

#[test]
fn nested_bundle_mutations_invalidate_verification_and_signature_failures_are_not_cached() {
    let mut fixture = Fixture::new();
    let root = fixture.directory.join("runtime");
    provision(&root, &[], false, &mut fixture).unwrap();
    provision(&root, &[], false, &mut fixture).unwrap();
    fixture.calls.clear();
    let nested = root.join("current/Codex.app/Contents/Resources/nested-fixture");
    fs::write(&nested, "mutation").unwrap();
    provision(&root, &[], false, &mut fixture).unwrap();
    assert!(fixture.calls.iter().any(|program| program.ends_with("codesign")));
    fixture.calls.clear();
    let tampered = root.join("current/Codex.app/tampered");
    fs::write(&tampered, "unsigned mutation").unwrap();
    for _ in 0..2 {
        assert!(provision(&root, &[], false, &mut fixture).unwrap_err().contains("invalid signature"));
    }
    assert_eq!(fixture.calls.iter().filter(|program| program.ends_with("codesign")).count(), 2);
}

#[test]
fn corrupt_future_or_symlinked_verification_records_force_full_verification() {
    let mut fixture = Fixture::new();
    let root = fixture.directory.join("runtime");
    provision(&root, &[], false, &mut fixture).unwrap();
    provision(&root, &[], false, &mut fixture).unwrap();
    let path = root.join(".startup-cache/verification-v1.json");
    let mut record: VerificationRecord = crate::startup_cache::read(&path).unwrap();
    record.verified_at = crate::startup_cache::now() + 60;
    crate::startup_cache::write(&path, &record).unwrap();
    fixture.calls.clear();
    provision(&root, &[], false, &mut fixture).unwrap();
    assert_eq!(fixture.calls.iter().filter(|program| program.ends_with("codesign")).count(), 2);
    fs::write(&path, "invalid JSON").unwrap();
    fixture.calls.clear();
    provision(&root, &[], false, &mut fixture).unwrap();
    assert_eq!(fixture.calls.iter().filter(|program| program.ends_with("codesign")).count(), 2);
    let copied = fixture.directory.join("saved-verification.json");
    fs::rename(&path, &copied).unwrap();
    std::os::unix::fs::symlink(copied, &path).unwrap();
    fixture.calls.clear();
    provision(&root, &[], false, &mut fixture).unwrap();
    assert_eq!(fixture.calls.iter().filter(|program| program.ends_with("codesign")).count(), 2);
    assert!(fs::symlink_metadata(path).unwrap().is_file());
}

#[test]
fn explicit_refresh_bypasses_a_fresh_verification_cache() {
    let mut fixture = Fixture::new();
    let root = fixture.directory.join("runtime");
    provision(&root, &[], false, &mut fixture).unwrap();
    provision(&root, &[], false, &mut fixture).unwrap();
    fixture.calls.clear();
    cached(&root, &mut fixture, true).unwrap().unwrap();
    assert_eq!(fixture.calls.iter().filter(|program| program.ends_with("codesign")).count(), 2);
}

#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "catalog-only timing against an explicitly selected installed signed provider"]
async fn installed_verified_startup_cache_timings() {
    use std::time::Instant;
    let receipt_path = PathBuf::from(std::env::var_os("NANOCODEX_TEST_INSTALLED_COMPUTER_RECEIPT").expect("explicit installed provider receipt"));
    let output = PathBuf::from(std::env::var_os("NANOCODEX_TEST_COMPUTER_OUTPUT").expect("private measurement output directory"));
    let receipt: serde_json::Value = serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
    let root = receipt_path.parent().unwrap();
    let app = root.join("current/Codex.app").canonicalize().unwrap();
    fs::create_dir_all(&output).unwrap();
    let cache_root = output.join(format!("cache-probe-{}", nonce()));
    let mut results = Vec::new();
    for run in 1..=4 {
        let started = Instant::now();
        let cancellation = Cancellation::new();
        let (build, fingerprint) = verified_cached(&cache_root, &app, &mut System::new(cancellation.flag()), false).unwrap();
        let verified_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(build, SUPPORTED_GUI_BUILD);
        let mut config = crate::provision::config_from_receipt(&receipt).unwrap();
        config.catalog_cache = Some(crate::startup_cache::CatalogCache::managed(&cache_root, config.executable.parent().unwrap(), &fingerprint.unwrap()));
        let started = Instant::now();
        let computer = crate::ComputerTools::connect(config).await.unwrap();
        let catalog_ms = started.elapsed().as_secs_f64() * 1000.0;
        let row = serde_json::json!({"run":run, "verified_ms":verified_ms, "catalog_ms":catalog_ms, "tool_count":computer.catalog().len()});
        println!("{row}");
        results.push(row);
    }
    fs::write(output.join("installed-rust-cache-timings.json"), serde_json::to_vec_pretty(&results).unwrap()).unwrap();
    fs::remove_dir_all(cache_root).unwrap();
}

#[test]
fn bundle_mutation_during_verification_never_publishes_a_successful_cache() {
    struct Mutating<'a> { fixture: &'a mut Fixture }
    impl Commands for Mutating<'_> {
        fn run(&mut self, program: &str, args: &[OsString]) -> Result<String, String> {
            let result = self.fixture.run(program, args)?;
            if program == "/usr/bin/codesign" && args[0] == "--verify" {
                let app = PathBuf::from(args.last().unwrap());
                fs::write(app.join("Contents/Resources/codex"), "changed after verification").unwrap();
            }
            Ok(result)
        }
    }
    let mut fixture = Fixture::new();
    let root = fixture.directory.join("runtime");
    let app = fixture.directory.join("Codex.app");
    Fixture::app(&app);
    let error = verified_cached(&root, &app, &mut Mutating { fixture: &mut fixture }, false).unwrap_err();
    assert!(error.contains("changed during signature verification"), "{error}");
    assert!(!root.join(".startup-cache/verification-v1.json").exists());
}

#[test]
fn system_drains_both_full_pipes_and_reports_normal_exit_status() {
    let cancellation = Cancellation::new();
    let mut system = System::new(cancellation.flag());
    let text = system.run("/bin/sh", &[
        "-c".into(),
        "i=0; while [ \"$i\" -lt 8192 ]; do printf 'stdout-line-0123456789\\n'; printf 'stderr-line-0123456789\\n' >&2; i=$((i + 1)); done".into(),
    ]).unwrap();
    assert_eq!(text, format!("{}{}", "stdout-line-0123456789\n".repeat(8192), "stderr-line-0123456789\n".repeat(8192)));
    let error = system.run("/bin/sh", &["-c".into(), "printf expected-failure >&2; exit 7".into()]).unwrap_err();
    assert!(error.contains("7") && error.contains("expected-failure"), "{error}");
}

fn assert_direct_child_reaped(pid: libc::pid_t) {
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) }, -1);
    assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ECHILD));
}

fn assert_descendant_stopped(pid: libc::pid_t) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        if unsafe { libc::kill(pid, 0) } == -1 {
            assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
            return;
        }
        // Some Linux test containers do not reap adopted grandchildren promptly.
        // A zombie is stopped; only its adopting parent can reap it. macOS init
        // reaps these descendants, so assert disappearance there as well.
        #[cfg(target_os = "linux")]
        if fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| stat.rsplit_once(") ").is_some_and(|(_, tail)| tail.starts_with("Z "))) {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "descendant {pid} survived cancellation");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[tokio::test]
async fn aborted_blocking_command_reaps_own_group_and_descendants_within_shutdown_budget() {
    use std::time::{Duration, Instant};
    // Cover both a running group leader and an exited leader whose descendant
    // still owns both pipes. Never signal the test harness's process group.
    for parent_exits in [false, true] {
        let fixture = Fixture::new();
        let pid_file = fixture.directory.join("owned-pids");
        let child_file = pid_file.clone();
        let (completed, result) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let cancellation = Cancellation::new();
            let mut system = System::new(cancellation.flag());
            tokio::task::spawn_blocking(move || {
                let script = format!("sleep 60 & descendant=$!; printf '%s %s\\n' \"$$\" \"$descendant\" > \"$1\"; {}", if parent_exits { "exit 0" } else { "wait \"$descendant\"" });
                let result = system.run("/bin/sh", &["-c".into(), script.into(), "owned-cancel-test".into(), child_file.into_os_string()]);
                let _ = completed.send(result);
            }).await.unwrap();
            drop(cancellation);
        });
        let pids = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(text) = fs::read_to_string(&pid_file) {
                    let pids: Vec<libc::pid_t> = text.split_whitespace().map(|pid| pid.parse().unwrap()).collect();
                    if pids.len() == 2 { break pids; }
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }).await.unwrap();
        // macOS getpgid reports ESRCH for an exited, still-unreaped leader.
        if !parent_exits { assert_eq!(unsafe { libc::getpgid(pids[0]) }, pids[0]); }
        assert_eq!(unsafe { libc::getpgid(pids[1]) }, pids[0]);
        assert_ne!(unsafe { libc::getpgrp() }, pids[0]);
        if parent_exits { tokio::time::sleep(Duration::from_millis(10)).await; }
        let started = Instant::now();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let error = tokio::time::timeout(Duration::from_millis(100), result).await.unwrap().unwrap().unwrap_err();
        assert_eq!(error, CANCELLED);
        assert_direct_child_reaped(pids[0]);
        let elapsed = started.elapsed();
        assert!(elapsed < Duration::from_millis(100), "reaping took {elapsed:?}");
        assert_descendant_stopped(pids[1]);
        println!("owned command cancellation parent_exits={parent_exits}: {:.3} ms; direct child reaped, descendant gone", elapsed.as_secs_f64() * 1000.0);
    }
}

#[test]
fn dropping_unreaped_command_stops_descendants_even_after_leader_exits() {
    use std::os::unix::process::CommandExt;
    let fixture = Fixture::new();
    let pid_file = fixture.directory.join("descendant-pid");
    let child = Command::new("/bin/sh")
        .args(["-c", "sleep 60 & printf '%s\\n' \"$!\" > \"$1\"; exit 0", "owned-drop-test"])
        .arg(&pid_file).process_group(0)
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
        .spawn().unwrap();
    let child = OwnedCommand::new(child);
    let parent = child.child.id() as libc::pid_t;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let descendant = loop {
        if let Ok(text) = fs::read_to_string(&pid_file) {
            if let Ok(pid) = text.trim().parse::<libc::pid_t>() { break pid; }
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    std::thread::sleep(std::time::Duration::from_millis(10));
    drop(child);
    assert_direct_child_reaped(parent);
    assert_descendant_stopped(descendant);
}

#[test]
fn cancelled_download_detaches_and_retains_staging_if_detach_fails() {
    struct Cancelling<'a> { fixture: &'a mut Fixture, cancelled: bool, fail_detach: bool, detached: bool }
    impl Commands for Cancelling<'_> {
        fn check_cancelled(&self) -> Result<(), String> {
            if self.cancelled { Err(CANCELLED.into()) } else { Ok(()) }
        }
        fn run(&mut self, program: &str, args: &[OsString]) -> Result<String, String> {
            if program == "/usr/bin/hdiutil" && args[0] == "detach" {
                assert!(Path::new(args.last().unwrap()).join("ChatGPT.app").is_dir());
                self.detached = true;
                return if self.fail_detach { Err("test detach failure".into()) } else { Ok(String::new()) };
            }
            self.check_cancelled()?;
            let result = self.fixture.run(program, args)?;
            if program == "/usr/bin/hdiutil" && args[0] == "attach" {
                self.cancelled = true;
                return Err(CANCELLED.into());
            }
            Ok(result)
        }
    }
    for fail_detach in [false, true] {
        let mut fixture = Fixture::new();
        let root = fixture.directory.join("runtime");
        let mut commands = Cancelling { fixture: &mut fixture, cancelled: false, fail_detach, detached: false };
        let error = provision(&root, &[], false, &mut commands).unwrap_err();
        assert!(commands.detached);
        assert!(!root.join("current").exists() && !root.join("provider.json").exists());
        let retained = fs::read_dir(&root).unwrap().filter_map(Result::ok).any(|entry| entry.file_name().to_string_lossy().starts_with(".staging-"));
        assert_eq!(retained, fail_detach);
        assert!(error.contains(if fail_detach { "staging retained" } else { CANCELLED }), "{error}");
    }
}

#[test]
fn cancelled_cached_verification_and_receipt_commit_preserve_previous_publication() {
    struct Cancelling<'a> { fixture: &'a mut Fixture, checks: std::cell::Cell<usize>, cancel_at: usize }
    impl Commands for Cancelling<'_> {
        fn check_cancelled(&self) -> Result<(), String> {
            let checks = self.checks.get() + 1;
            self.checks.set(checks);
            if checks >= self.cancel_at { Err(CANCELLED.into()) } else { Ok(()) }
        }
        fn run(&mut self, program: &str, args: &[OsString]) -> Result<String, String> {
            self.check_cancelled()?;
            self.fixture.run(program, args)
        }
    }
    let mut fixture = Fixture::new();
    let root = fixture.directory.join("runtime");
    let receipt = provision(&root, &[], false, &mut fixture).unwrap();
    provision(&root, &[], false, &mut fixture).unwrap();
    let cache = fs::read(root.join(".startup-cache/verification-v1.json")).unwrap();
    let previous = b"prior receipt must survive cancellation";
    fs::write(root.join("provider.json"), previous).unwrap();
    let current = fs::read_link(root.join("current")).unwrap();
    // Cancellation after fingerprint, after host preparation, and just before
    // the receipt's atomic rename must all preserve the earlier publication.
    for cancel_at in [3, 5, 7] {
        fixture.calls.clear();
        let mut commands = Cancelling { fixture: &mut fixture, checks: std::cell::Cell::new(0), cancel_at };
        assert_eq!(provision(&root, &[], false, &mut commands).unwrap_err(), CANCELLED);
        assert_eq!(fs::read(root.join("provider.json")).unwrap(), previous);
        assert_eq!(fs::read_link(root.join("current")).unwrap(), current);
        assert_eq!(fs::read(root.join(".startup-cache/verification-v1.json")).unwrap(), cache);
        assert!(fixture.calls.is_empty(), "warm cancellation must not run verification commands");
        assert!(!fs::read_dir(&root).unwrap().filter_map(Result::ok).any(|entry| entry.file_name().to_string_lossy().starts_with(".provider-")));
    }
    assert!(Path::new(receipt["executable"].as_str().unwrap()).is_file());
}
