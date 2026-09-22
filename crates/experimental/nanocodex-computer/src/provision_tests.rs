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
