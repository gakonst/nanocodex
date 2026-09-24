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
    #[cfg(target_os = "linux")]
    return linux::configure(action, root, nightly);
    #[cfg(target_os = "windows")]
    return windows::configure(action, root, nightly);
    #[allow(unreachable_code)]
    Err(eyre::eyre!("unsupported automatic update platform"))
}

/// Install the default schedule once, preserving an existing schedule and channel.
pub(super) fn ensure_default(root: &Path, nightly: bool) -> Result<()> {
    #[cfg(target_os = "macos")]
    return macos::ensure_default(root, nightly);
    #[cfg(target_os = "linux")]
    return linux::ensure_default(root, nightly);
    #[cfg(target_os = "windows")]
    return windows::ensure_default(root, nightly);
    #[allow(unreachable_code)]
    {
        let _ = (root, nightly);
        Err(eyre::eyre!("unsupported automatic update platform"))
    }
}

fn opt_out_path(root: &Path) -> std::path::PathBuf {
    root.join("automatic-updates-disabled")
}

fn entry_exists(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn should_install_default(root: &Path, schedule: &Path) -> Result<bool> {
    // Even a dangling link counts as an existing entry: never replace it implicitly.
    Ok(!entry_exists(&opt_out_path(root))? && !entry_exists(schedule)?)
}

fn record_opt_out(root: &Path) -> Result<()> {
    std::fs::create_dir_all(root)?;
    let marker = tempfile::NamedTempFile::new_in(root)?;
    marker.as_file().sync_all()?;
    marker.persist(opt_out_path(root))?;
    Ok(())
}

fn clear_opt_out(root: &Path) -> Result<()> {
    match std::fs::remove_file(opt_out_path(root)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn supported(os: &str) -> Result<()> {
    if !matches!(os, "macos" | "linux" | "windows") {
        bail!("automatic updates are unsupported on {os}");
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

#[cfg(any(target_os = "linux", test))]
#[cfg_attr(all(test, not(target_os = "linux")), allow(dead_code))]
mod linux {
    use super::*;
    use eyre::{WrapErr, eyre};
    use std::{
        fs,
        io::Write as _,
        path::{Path, PathBuf},
        process::{Command, Output},
    };

    const SERVICE: &str = "nanocodex-update.service";
    const TIMER: &str = "nanocodex-update.timer";
    const OWNER: &str = "nanocodex.native-updater.v1";

    fn home() -> Result<PathBuf> {
        let home = PathBuf::from(
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .ok_or_else(|| eyre!("HOME is not set"))?,
        );
        if !home.is_absolute() {
            bail!("HOME must be absolute");
        }
        Ok(home)
    }

    fn paths(home: &Path) -> (PathBuf, PathBuf) {
        let directory = home.join(".config/systemd/user");
        (directory.join(SERVICE), directory.join(TIMER))
    }

    fn quote(value: &str) -> Result<String> {
        if value
            .chars()
            .any(|character| character < ' ' || character == '\u{7f}')
        {
            bail!("automatic update paths contain a control character");
        }
        Ok(format!(
            "\"{}\"",
            value
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('%', "%%")
        ))
    }

    fn path(path: &Path) -> Result<&str> {
        if !path.is_absolute() {
            bail!(
                "automatic update paths must be absolute: {}",
                path.display()
            );
        }
        path.to_str()
            .ok_or_else(|| eyre!("automatic update paths must be UTF-8"))
    }

    pub(super) fn render_service(root: &Path, home: &Path, nightly: bool) -> Result<String> {
        let executable = quote(path(&root.join("updater/nanocodex"))?)?;
        let root_environment = quote(&format!("NANOCODEX_DIR={}", path(root)?))?;
        let home_environment = quote(&format!("HOME={}", path(home)?))?;
        let nightly = if nightly { " --nightly" } else { "" };
        Ok(format!(
            "[Unit]\nDescription=Nanocodex automatic updates\nX-Nanocodex-ManagedBy={OWNER}\n\n[Service]\nType=oneshot\nEnvironment={home_environment}\nEnvironment={root_environment}\nExecStart={executable} update --background{nightly}\n"
        ))
    }

    pub(super) fn render_timer() -> String {
        format!(
            "[Unit]\nDescription=Run Nanocodex automatic updates hourly\nX-Nanocodex-ManagedBy={OWNER}\n\n[Timer]\nOnActiveSec=5m\nOnCalendar=hourly\nAccuracySec=1m\nPersistent=true\nUnit={SERVICE}\n\n[Install]\nWantedBy=timers.target\n"
        )
    }

    fn read_regular(path: &Path) -> Result<Option<String>> {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
            Ok(metadata) if !metadata.file_type().is_file() => {
                bail!(
                    "refusing non-regular automatic update unit {}",
                    path.display()
                )
            }
            Ok(_) => Ok(Some(fs::read_to_string(path)?)),
        }
    }

    fn existing(root: &Path, home: &Path) -> Result<bool> {
        let (service_path, timer_path) = paths(home);
        let service = read_regular(&service_path)?;
        let timer = read_regular(&timer_path)?;
        match (service, timer) {
            (None, None) => Ok(false),
            (Some(service), Some(timer))
                if (service == render_service(root, home, false)?
                    || service == render_service(root, home, true)?)
                    && timer == render_timer() =>
            {
                Ok(true)
            }
            _ => bail!(
                "refusing to modify incomplete, unmanaged, or different-installation systemd update units"
            ),
        }
    }

    fn save(path: &Path, contents: &str) -> Result<()> {
        let parent = path.parent().expect("systemd user unit has parent");
        fs::create_dir_all(parent)?;
        if fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.is_file()) {
            bail!(
                "refusing non-regular automatic update unit {}",
                path.display()
            );
        }
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(contents.as_bytes())?;
        temporary.as_file().sync_all()?;
        temporary.persist(path)?;
        Ok(())
    }

    fn systemctl(arguments: &[&str]) -> Result<Output> {
        Command::new("systemctl")
            .arg("--user")
            .args(arguments)
            .output()
            .wrap_err("Could not run the user systemd manager")
    }

    fn checked(arguments: &[&str], operation: &str) -> Result<()> {
        let output = systemctl(arguments)?;
        if !output.status.success() {
            bail!(
                "Could not {operation}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    pub(super) fn ensure_default(root: &Path, nightly: bool) -> Result<()> {
        if entry_exists(&opt_out_path(root))? {
            return Ok(());
        }
        let home = home()?;
        let (_, timer) = paths(&home);
        if should_install_default(root, &timer)? {
            configure(AutoUpdate::Enable, root, nightly)?;
        }
        Ok(())
    }

    pub(super) fn configure(action: AutoUpdate, root: &Path, nightly: bool) -> Result<()> {
        if !root.is_absolute() {
            bail!("automatic update root must be absolute");
        }
        let home = home()?;
        let (service_path, timer_path) = paths(&home);
        let installed = existing(root, &home)?;
        match action {
            AutoUpdate::Status => {
                let enabled = systemctl(&["is-enabled", TIMER])?.status.success();
                let active = systemctl(&["is-active", TIMER])?.status.success();
                println!(
                    "Automatic updates: configured={installed}, enabled={enabled}, active={active}\nTimer: {}",
                    timer_path.display()
                );
            }
            AutoUpdate::Enable => {
                let updater = root.join("updater/nanocodex");
                if !updater.is_file() {
                    bail!("managed updater is missing at {}", updater.display());
                }
                save(&service_path, &render_service(root, &home, nightly)?)?;
                save(&timer_path, &render_timer())?;
                checked(&["daemon-reload"], "reload user systemd units")?;
                checked(&["enable", "--now", TIMER], "enable automatic updates")?;
                clear_opt_out(root)?;
                println!("Automatic updates enabled every hour (nightly={nightly}).");
            }
            AutoUpdate::Disable => {
                record_opt_out(root)?;
                if installed {
                    checked(&["disable", "--now", TIMER], "disable automatic updates")?;
                    fs::remove_file(timer_path)?;
                    fs::remove_file(service_path)?;
                    checked(&["daemon-reload"], "reload user systemd units")?;
                }
                println!("Automatic updates disabled.");
            }
        }
        Ok(())
    }
}

#[cfg(any(target_os = "windows", test))]
#[cfg_attr(all(test, not(target_os = "windows")), allow(dead_code))]
mod windows {
    use super::*;
    use eyre::{WrapErr, eyre};
    use sha2::{Digest as _, Sha256};
    use std::{io::Write as _, path::Path, process::Command};

    const TASK: &str = r"\Nanocodex Update";
    const OWNER: &str = "nanocodex.native-updater.v1";

    fn xml(value: &str) -> Result<String> {
        if value
            .chars()
            .any(|character| character < ' ' && !matches!(character, '\n' | '\r' | '\t'))
        {
            bail!("Invalid XML control character");
        }
        Ok(value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;"))
    }

    fn path(path: &Path) -> Result<&str> {
        if !path.is_absolute() {
            bail!(
                "automatic update paths must be absolute: {}",
                path.display()
            );
        }
        path.to_str()
            .ok_or_else(|| eyre!("automatic update paths must be valid Unicode"))
    }

    fn owner(root: &Path) -> Result<String> {
        let digest = Sha256::digest(path(root)?.as_bytes());
        let digest = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Ok(format!("{OWNER}:{digest}"))
    }

    pub(super) fn render(root: &Path, user: &str, nightly: bool) -> Result<String> {
        let executable = xml(path(&root.join("updater/nanocodex.exe"))?)?;
        let working_directory = xml(path(root)?)?;
        let owner = owner(root)?;
        let user = xml(user)?;
        let arguments = if nightly {
            "update --background --nightly"
        } else {
            "update --background"
        };
        Ok(format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>{owner}</Description><URI>{TASK}</URI></RegistrationInfo>
  <Triggers>
    <LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger>
    <TimeTrigger><Repetition><Interval>PT1H</Interval><StopAtDurationEnd>false</StopAtDurationEnd></Repetition><StartBoundary>2020-01-01T00:05:00</StartBoundary><Enabled>true</Enabled></TimeTrigger>
  </Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
  <Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><StartWhenAvailable>true</StartWhenAvailable><RunOnlyIfNetworkAvailable>true</RunOnlyIfNetworkAvailable><AllowStartOnDemand>true</AllowStartOnDemand><Enabled>true</Enabled><Hidden>true</Hidden><ExecutionTimeLimit>PT30M</ExecutionTimeLimit></Settings>
  <Actions Context="Author"><Exec><Command>{executable}</Command><Arguments>{arguments}</Arguments><WorkingDirectory>{working_directory}</WorkingDirectory></Exec></Actions>
</Task>
"#
        ))
    }

    fn task_definition(root: &Path) -> Result<Option<String>> {
        let output = Command::new("schtasks.exe")
            .args(["/Query", "/TN", TASK, "/XML"])
            .output()
            .wrap_err("Could not inspect the Windows automatic update task")?;
        if !output.status.success() {
            return Ok(None);
        }
        let definition = crate::windows_hand::decode_task_xml(&output.stdout);
        if !definition.contains(&owner(root)?)
            || !definition.contains("<Arguments>update --background")
        {
            bail!(
                "refusing to modify an unmanaged or different-installation automatic update task"
            );
        }
        Ok(Some(definition))
    }

    fn checked(arguments: &[&str], operation: &str) -> Result<()> {
        let output = Command::new("schtasks.exe")
            .args(arguments)
            .output()
            .wrap_err_with(|| format!("Could not {operation}"))?;
        if !output.status.success() {
            bail!(
                "Could not {operation}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    pub(super) fn ensure_default(root: &Path, nightly: bool) -> Result<()> {
        if entry_exists(&opt_out_path(root))? {
            return Ok(());
        }
        if task_definition(root)?.is_none() {
            configure(AutoUpdate::Enable, root, nightly)?;
        }
        Ok(())
    }

    pub(super) fn configure(action: AutoUpdate, root: &Path, nightly: bool) -> Result<()> {
        if !root.is_absolute() {
            bail!("automatic update root must be absolute");
        }
        let definition = task_definition(root)?;
        match action {
            AutoUpdate::Status => {
                println!(
                    "Automatic updates: configured={}\nTask: {TASK}",
                    definition.is_some()
                );
            }
            AutoUpdate::Enable => {
                let updater = root.join("updater/nanocodex.exe");
                if !updater.is_file() {
                    bail!("managed updater is missing at {}", updater.display());
                }
                let user = crate::windows_hand::current_user()?;
                let contents = render(root, &user, nightly)?;
                let mut temporary = tempfile::Builder::new().suffix(".xml").tempfile()?;
                temporary.write_all(contents.as_bytes())?;
                temporary.as_file().sync_all()?;
                let path = temporary
                    .path()
                    .to_str()
                    .ok_or_else(|| eyre!("Temporary task path must be valid Unicode"))?;
                checked(
                    &["/Create", "/TN", TASK, "/XML", path, "/F"],
                    "install the Windows automatic update task",
                )?;
                clear_opt_out(root)?;
                println!("Automatic updates enabled every hour (nightly={nightly}).");
            }
            AutoUpdate::Disable => {
                record_opt_out(root)?;
                if definition.is_some() {
                    checked(
                        &["/Delete", "/TN", TASK, "/F"],
                        "remove the Windows automatic update task",
                    )?;
                }
                println!("Automatic updates disabled.");
            }
        }
        Ok(())
    }
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
    fn linux_schedule_runs_the_native_updater_without_a_shell() {
        let service = linux::render_service(
            Path::new("/home/test/.nanocodex"),
            Path::new("/home/test"),
            true,
        )
        .unwrap();
        assert!(service.contains("X-Nanocodex-ManagedBy=nanocodex.native-updater.v1"));
        assert!(service.contains(
            "ExecStart=\"/home/test/.nanocodex/updater/nanocodex\" update --background --nightly"
        ));
        assert!(!service.contains("sh -c"));
        let timer = linux::render_timer();
        assert!(timer.contains("OnCalendar=hourly"));
        assert!(timer.contains("Persistent=true"));
    }

    #[test]
    fn windows_schedule_runs_the_native_updater_without_secrets_or_powershell() {
        #[cfg(target_os = "windows")]
        let root = Path::new(r"C:\Users\test\.nanocodex");
        #[cfg(not(target_os = "windows"))]
        let root = Path::new(r"/C:/Users/test/.nanocodex");
        let task = windows::render(root, r"DESKTOP\test", false).unwrap();
        assert!(task.contains("nanocodex.native-updater.v1"));
        assert!(task.contains("nanocodex.exe</Command>"));
        assert!(task.contains("<Arguments>update --background</Arguments>"));
        assert!(task.contains("<Interval>PT1H</Interval>"));
        assert!(!task.contains("powershell"));
        assert!(!task.contains("credential"));
    }

    #[test]
    fn rejects_unsupported_platforms_and_invalid_paths() {
        assert!(supported("plan9").is_err());
        assert!(supported("linux").is_ok());
        assert!(supported("windows").is_ok());
        assert!(supported("macos").is_ok());
        assert!(render(Path::new("relative"), Path::new("/home"), false).is_err());
        assert!(render(Path::new("/invalid\u{1}"), Path::new("/home"), false).is_err());
    }
}
