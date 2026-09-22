//! Native, per-user scheduling. The Rust update coordinator defers activation
//! while a Hand is loaded; restarting a running Hand is an explicit operation.
use std::path::Path;

use clap::ValueEnum;
use eyre::{Result, bail};

#[derive(Debug, Clone, ValueEnum)]
pub(super) enum AutoUpdate {
    Enable,
    Disable,
    Status,
}

pub(super) fn configure(action: AutoUpdate, root: &Path, nightly: bool) -> Result<()> {
    supported(std::env::consts::OS)?;
    #[cfg(target_os = "macos")]
    return macos::configure(action, root, nightly);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (action, root, nightly);
        unreachable!("unsupported platforms return above")
    }
}

/// Install the default schedule once, preserving an existing schedule and channel.
pub(super) fn ensure_default(root: &Path, nightly: bool) -> Result<()> {
    #[cfg(target_os = "macos")]
    return macos::ensure_default(root, nightly);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (root, nightly);
        Ok(())
    }
}

#[cfg(any(target_os = "macos", test))]
fn opt_out_path(root: &Path) -> std::path::PathBuf {
    root.join("automatic-updates-disabled")
}

#[cfg(any(target_os = "macos", test))]
fn entry_exists(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(any(target_os = "macos", test))]
fn should_install_default(root: &Path, schedule: &Path) -> Result<bool> {
    // Even a dangling link counts as an existing entry: never replace it implicitly.
    Ok(!entry_exists(&opt_out_path(root))? && !entry_exists(schedule)?)
}

#[cfg(any(target_os = "macos", test))]
fn record_opt_out(root: &Path) -> Result<()> {
    std::fs::create_dir_all(root)?;
    let marker = tempfile::NamedTempFile::new_in(root)?;
    marker.as_file().sync_all()?;
    marker.persist(opt_out_path(root))?;
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
fn clear_opt_out(root: &Path) -> Result<()> {
    match std::fs::remove_file(opt_out_path(root)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn supported(os: &str) -> Result<()> {
    if os != "macos" {
        bail!("automatic updates are only supported on macOS (current platform: {os})");
    }
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
const LABEL: &str = "com.nanocodex.update";
#[cfg(any(target_os = "macos", test))]
const OWNER: &str = "nanocodex.native-updater.v1";

#[cfg(any(target_os = "macos", test))]
fn path_text(path: &Path) -> Result<&str> {
    if !path.is_absolute() {
        bail!(
            "automatic update paths must be absolute: {}",
            path.display()
        );
    }
    path.to_str()
        .ok_or_else(|| eyre::eyre!("automatic update paths must be UTF-8"))
}

#[cfg(any(target_os = "macos", test))]
fn escape(value: &str) -> Result<String> {
    if value.chars().any(|c| {
        !(matches!(c, '\t' | '\n' | '\r') || c >= ' ' && c != '\u{fffe}' && c != '\u{ffff}')
    }) {
        bail!("automatic update paths contain characters invalid in XML");
    }
    Ok(value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;"))
}

#[cfg(any(target_os = "macos", test))]
fn render(root: &Path, home: &Path, nightly: bool) -> Result<String> {
    let root_text = escape(path_text(root)?)?;
    let home = escape(path_text(home)?)?;
    let executable = escape(path_text(&root.join("updater/nanocodex"))?)?;
    let log = escape(path_text(&root.join("logs/update.log"))?)?;
    let nightly = if nightly {
        "<string>--nightly</string>"
    } else {
        ""
    };
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{LABEL}</string>
<key>NanocodexManagedBy</key><string>{OWNER}</string>
<key>ProgramArguments</key><array><string>{executable}</string><string>update</string><string>--background</string>{nightly}</array>
<key>StartInterval</key><integer>3600</integer>
<key>RunAtLoad</key><true/>
<key>EnvironmentVariables</key><dict><key>HOME</key><string>{home}</string><key>NANOCODEX_DIR</key><string>{root_text}</string></dict>
<key>StandardOutPath</key><string>{log}</string>
<key>StandardErrorPath</key><string>{log}</string>
</dict></plist>
"#
    ))
}

#[cfg(any(target_os = "macos", test))]
fn managed(value: &serde_json::Value, root: &Path) -> Result<()> {
    let executable = root.join("updater/nanocodex");
    let args = &value["ProgramArguments"];
    let base = serde_json::json!([path_text(&executable)?, "update", "--background"]);
    let nightly = serde_json::json!([
        path_text(&executable)?,
        "update",
        "--background",
        "--nightly"
    ]);
    if value["Label"] != LABEL
        || value["NanocodexManagedBy"] != OWNER
        || value["EnvironmentVariables"]["NANOCODEX_DIR"] != path_text(root)?
        || (args != &base && args != &nightly)
    {
        bail!(
            "refusing to modify an unmanaged or different-installation automatic update LaunchAgent"
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use eyre::Context;
    use std::{
        fs,
        io::Write,
        path::PathBuf,
        process::{Command, Output},
    };

    fn checked(output: Output, operation: &str) -> Result<()> {
        if !output.status.success() {
            bail!(
                "{operation} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    // Reject links and foreign plists before any launchctl or filesystem mutation.
    fn existing(path: &Path, root: &Path) -> Result<Option<serde_json::Value>> {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
            Ok(metadata) if !metadata.file_type().is_file() => {
                bail!("refusing non-regular LaunchAgent {}", path.display())
            }
            Ok(_) => {}
        }
        let output = Command::new("/usr/bin/plutil")
            .args(["-convert", "json", "-o", "-", "--"])
            .arg(path)
            .output()?;
        if !output.status.success() {
            bail!("cannot parse existing LaunchAgent {}", path.display());
        }
        let value = serde_json::from_slice(&output.stdout)?;
        managed(&value, root)?;
        Ok(Some(value))
    }

    pub(super) fn ensure_default(root: &Path, nightly: bool) -> Result<()> {
        // An explicit opt-out should not require HOME or spawn any subprocesses.
        if entry_exists(&opt_out_path(root))? {
            return Ok(());
        }
        let home =
            PathBuf::from(std::env::var_os("HOME").ok_or_else(|| eyre::eyre!("HOME is not set"))?);
        let path = home
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist"));
        if should_install_default(root, &path)? {
            configure(AutoUpdate::Enable, root, nightly)?;
        }
        Ok(())
    }

    pub(super) fn configure(action: AutoUpdate, root: &Path, nightly: bool) -> Result<()> {
        let home =
            PathBuf::from(std::env::var_os("HOME").ok_or_else(|| eyre::eyre!("HOME is not set"))?);
        let plist = render(root, &home, nightly)?;
        let path = home
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist"));
        let settings = existing(&path, root)?;
        let uid = Command::new("/usr/bin/id").arg("-u").output()?;
        if !uid.status.success() {
            bail!("failed to determine current user ID");
        }
        let uid = std::str::from_utf8(&uid.stdout)?.trim();
        if uid.is_empty() || !uid.bytes().all(|b| b.is_ascii_digit()) {
            bail!("invalid current user ID");
        }
        let domain = format!("gui/{uid}");
        let service = format!("{domain}/{LABEL}");
        let loaded = Command::new("/bin/launchctl")
            .args(["print", &service])
            .output()?
            .status
            .success();
        match action {
            AutoUpdate::Status => {
                println!(
                    "Automatic updates: configured={}, loaded={loaded}\nLaunchAgent: {}",
                    settings.is_some(),
                    path.display()
                );
                if let Some(settings) = settings {
                    println!("{}", serde_json::to_string_pretty(&settings)?);
                }
            }
            AutoUpdate::Enable => {
                if loaded && settings.is_none() {
                    bail!(
                        "refusing to replace a loaded LaunchAgent without a managed configuration"
                    );
                }
                if !root.join("updater/nanocodex").is_file() {
                    bail!(
                        "managed updater is missing at {}",
                        root.join("updater/nanocodex").display()
                    );
                }
                fs::create_dir_all(root.join("logs"))?;
                let parent = path.parent().expect("LaunchAgent has parent");
                fs::create_dir_all(parent)?;
                let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
                temporary.write_all(plist.as_bytes())?;
                temporary.as_file().sync_all()?;
                if loaded {
                    checked(
                        Command::new("/bin/launchctl")
                            .args(["bootout", &service])
                            .output()?,
                        "unload automatic updates",
                    )?;
                }
                temporary
                    .persist(&path)
                    .wrap_err("failed to atomically save automatic update LaunchAgent")?;
                checked(
                    Command::new("/bin/launchctl")
                        .args(["enable", &service])
                        .output()?,
                    "enable automatic updates",
                )?;
                checked(
                    Command::new("/bin/launchctl")
                        .args(["bootstrap", &domain])
                        .arg(&path)
                        .output()?,
                    "load automatic updates",
                )?;
                clear_opt_out(root)?;
                println!("Automatic updates enabled every 3600 seconds (nightly={nightly}).");
            }
            AutoUpdate::Disable => {
                if loaded && settings.is_none() {
                    bail!("refusing to unload a LaunchAgent without a managed configuration");
                }
                // Persist user intent before unloading so a partial failure cannot
                // cause a later startup to silently re-enable automatic updates.
                record_opt_out(root)?;
                if loaded {
                    checked(
                        Command::new("/bin/launchctl")
                            .args(["bootout", &service])
                            .output()?,
                        "unload automatic updates",
                    )?;
                }
                if settings.is_some() {
                    fs::remove_file(&path)?;
                }
                println!("Automatic updates disabled.");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_preserves_schedules_and_persistent_opt_out() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("install");
        let schedule = directory.path().join("schedule.plist");
        assert!(should_install_default(&root, &schedule).unwrap());
        for nightly in [false, true] {
            let original = render(&root, directory.path(), nightly).unwrap();
            std::fs::write(&schedule, &original).unwrap();
            assert!(!should_install_default(&root, &schedule).unwrap());
            assert_eq!(std::fs::read_to_string(&schedule).unwrap(), original);
        }
        record_opt_out(&root).unwrap();
        std::fs::remove_file(&schedule).unwrap();
        assert!(!should_install_default(&root, &schedule).unwrap());
        // Repeating disable is safe; the marker survives removal of the schedule.
        record_opt_out(&root).unwrap();
        assert!(!should_install_default(&root, &schedule).unwrap());
        clear_opt_out(&root).unwrap();
        assert!(should_install_default(&root, &schedule).unwrap());
        clear_opt_out(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn default_policy_preserves_dangling_schedule_links() {
        let directory = tempfile::tempdir().unwrap();
        let schedule = directory.path().join("schedule.plist");
        std::os::unix::fs::symlink(directory.path().join("missing"), &schedule).unwrap();
        assert!(!should_install_default(directory.path(), &schedule).unwrap());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn ensure_default_is_noop_on_other_platforms() {
        let directory = tempfile::tempdir().unwrap();
        ensure_default(directory.path(), true).unwrap();
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn escapes_paths_and_renders_exact_schedule() {
        let xml = render(
            Path::new("/a & <b> \"c\" 'd'"),
            Path::new("/Users/a & b"),
            false,
        )
        .unwrap();
        assert!(xml.contains("<string>/a &amp; &lt;b&gt; &quot;c&quot; &apos;d&apos;/updater/nanocodex</string><string>update</string><string>--background</string></array>"));
        assert!(xml.contains("<key>StartInterval</key><integer>3600</integer>"));
        assert!(xml.contains("<key>RunAtLoad</key><true/>"));
        assert_eq!(xml.matches("/logs/update.log</string>").count(), 2);
        assert!(xml.contains("<key>HOME</key><string>/Users/a &amp; b</string>"));
        assert!(!xml.contains("--nightly"));
    }

    #[test]
    fn nightly_is_a_separate_argument_and_environment_is_allowlisted() {
        let xml = render(Path::new("/install"), Path::new("/home"), true).unwrap();
        assert!(xml.contains("<string>--background</string><string>--nightly</string></array>"));
        let environment = xml
            .split("<key>EnvironmentVariables</key><dict>")
            .nth(1)
            .unwrap()
            .split("</dict>")
            .next()
            .unwrap();
        assert_eq!(
            environment,
            "<key>HOME</key><string>/home</string><key>NANOCODEX_DIR</key><string>/install</string>"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_plist_parser_roundtrips_escaped_paths() {
        use std::io::Write;
        let root = Path::new("/install & <tag> \"quoted\" 'apostrophe'");
        for nightly in [false, true] {
            let mut file = tempfile::NamedTempFile::new().unwrap();
            file.write_all(
                render(root, Path::new("/Users/test"), nightly)
                    .unwrap()
                    .as_bytes(),
            )
            .unwrap();
            let output = std::process::Command::new("/usr/bin/plutil")
                .args(["-convert", "json", "-o", "-", "--"])
                .arg(file.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            managed(&value, root).unwrap();
            assert_eq!(value["StartInterval"], 3600);
            assert_eq!(value["RunAtLoad"], true);
            assert_eq!(value["EnvironmentVariables"].as_object().unwrap().len(), 2);
            assert_eq!(
                value["ProgramArguments"].as_array().unwrap().len(),
                if nightly { 4 } else { 3 }
            );
        }
    }

    #[test]
    fn ownership_requires_marker_root_and_exact_command() {
        let mut value = serde_json::json!({"Label": LABEL, "NanocodexManagedBy": OWNER,
            "ProgramArguments": ["/install/updater/nanocodex", "update", "--background"],
            "EnvironmentVariables": {"NANOCODEX_DIR": "/install"}});
        assert!(managed(&value, Path::new("/install")).is_ok());
        assert!(managed(&value, Path::new("/other")).is_err());
        value["ProgramArguments"][0] = "sh".into();
        assert!(managed(&value, Path::new("/install")).is_err());
        value["NanocodexManagedBy"] = serde_json::Value::Null;
        assert!(managed(&value, Path::new("/install")).is_err());
    }

    #[test]
    fn rejects_unsupported_platforms_and_invalid_paths() {
        assert!(
            supported("linux")
                .unwrap_err()
                .to_string()
                .contains("only supported on macOS")
        );
        assert!(supported("windows").is_err());
        assert!(supported("macos").is_ok());
        assert!(render(Path::new("relative"), Path::new("/home"), false).is_err());
        assert!(render(Path::new("/invalid\u{1}"), Path::new("/home"), false).is_err());
    }
}
