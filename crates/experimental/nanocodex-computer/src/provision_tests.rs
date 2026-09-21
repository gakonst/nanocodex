use super::*;
use std::process::Command;

struct Fixture {
    directory: PathBuf,
    fail_download: bool,
    bad_copy: bool,
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
                "1234"
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
    assert_eq!(
        receipt["executable"],
        root.join("current/cua-provider").to_str().unwrap()
    );
    assert!(!f.calls.iter().any(|s| s.ends_with("curl")));
    let script = fs::read_to_string(root.join("current/cua-provider")).unwrap();
    assert!(script.contains("CODEX_CLI_PATH="));
    assert!(script.contains("NODE_REPL_UNTRUSTED_ENV_ALLOWLIST=SKY_CUA_SERVICE_PATH"));
    assert!(
        Command::new("/bin/sh")
            .args(["-n"])
            .arg(root.join("current/cua-provider"))
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
    provision(&root, &[], false, &mut f).unwrap();
    assert!(root.join("current/cua-provider").is_file());
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
    assert!(root.join(previous).join("cua-provider").is_file());
}

#[test]
fn corrupted_cache_requires_explicit_repair_instead_of_another_backend() {
    let mut f = Fixture::new();
    let root = f.directory.join("runtime");
    provision(&root, &[], false, &mut f).unwrap();
    fs::write(root.join("current/cua-provider"), "modified").unwrap();
    f.calls.clear();
    assert!(
        provision(&root, &[], false, &mut f)
            .unwrap_err()
            .contains("--refresh")
    );
    assert!(f.calls.is_empty());
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
