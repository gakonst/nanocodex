//! User-owned macOS Hand service. Account credentials remain in their original file.
use eyre::{Result, WrapErr, bail, eyre};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
use tokio::process::Command;

const LABEL: &str = "com.nanocodex.hand";
#[derive(Debug, Serialize)]
pub(crate) struct ServiceStatus {
    pub installed: bool,
    pub loaded: bool,
    pub pid: Option<u32>,
    pub executable: Option<PathBuf>,
}
fn supported() -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!(
            "Local Hand service commands currently support macOS only; use hand add for SSH Linux setup"
        );
    }
    Ok(())
}
fn home() -> Result<PathBuf> {
    let path = PathBuf::from(std::env::var_os("HOME").ok_or_else(|| eyre!("HOME is unset"))?);
    if !path.is_absolute() {
        bail!("HOME must be absolute");
    }
    Ok(path)
}
fn plist_path() -> Result<PathBuf> {
    Ok(home()?
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}
async fn domain() -> Result<String> {
    supported()?;
    let out = Command::new("/usr/bin/id").arg("-u").output().await?;
    let uid = String::from_utf8(out.stdout)?.trim().parse::<u32>()?;
    if !out.status.success() || uid == 0 {
        bail!("Run Hand service commands as the desktop user, without sudo");
    }
    Ok(format!("gui/{uid}"))
}
async fn launch(args: &[&str]) -> Result<std::process::Output> {
    supported()?;
    Ok(Command::new("/bin/launchctl").args(args).output().await?)
}
async fn checked(args: &[&str]) -> Result<()> {
    if !launch(args).await?.status.success() {
        bail!("launchctl {} failed", args[0]);
    }
    Ok(())
}
pub(crate) async fn refuse_system_service() -> Result<()> {
    supported()?;
    if competing_owner(
        Path::new("/Library/LaunchDaemons/com.nanocodex.hand.plist").exists(),
        launch(&["print", &format!("system/{LABEL}")])
            .await?
            .status
            .success(),
    ) {
        bail!(
            "A system Hand service exists; remove that competing owner before managing the user Hand service"
        );
    }
    Ok(())
}
fn regular_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() {
        bail!("Expected a regular file: {}", path.display());
    }
    Ok(())
}
async fn read_plist() -> Result<Value> {
    read_plist_path(&plist_path()?).await
}
async fn read_plist_path(path: &Path) -> Result<Value> {
    regular_file(path)?;
    let out = Command::new("/usr/bin/plutil")
        .args(["-convert", "json", "-o", "-"])
        .arg(path)
        .output()
        .await?;
    if !out.status.success() {
        bail!("Could not read Hand LaunchAgent plist");
    }
    let value: Value = serde_json::from_slice(&out.stdout)?;
    validate_plist(&value)?;
    Ok(value)
}
fn xml(value: &str) -> Result<String> {
    if value
        .chars()
        .any(|c| c < ' ' && !matches!(c, '\n' | '\r' | '\t'))
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
fn render(value: &Value) -> Result<String> {
    Ok(match value {
        Value::String(s) => format!("<string>{}</string>", xml(s)?),
        Value::Bool(b) => format!("<{b}/>"),
        Value::Number(n) if n.is_i64() || n.is_u64() => format!("<integer>{n}</integer>"),
        Value::Array(items) => format!(
            "<array>{}</array>",
            items
                .iter()
                .map(render)
                .collect::<Result<Vec<_>>>()?
                .join("")
        ),
        Value::Object(items) => format!(
            "<dict>{}</dict>",
            items
                .iter()
                .map(|(key, v)| Ok(format!("<key>{}</key>{}", xml(key)?, render(v)?)))
                .collect::<Result<Vec<_>>>()?
                .join("")
        ),
        _ => bail!("Unsupported Hand plist value"),
    })
}
fn write_plist(value: &Value) -> Result<()> {
    let path = plist_path()?;
    let parent = path.parent().unwrap();
    fs::create_dir_all(parent)?;
    if path.symlink_metadata().is_ok() {
        regular_file(&path)?;
    }
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    writeln!(
        file,
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\">{}</plist>",
        render(value)?
    )?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}
fn executable(path: &Path) -> Result<PathBuf> {
    let path = fs::canonicalize(path).wrap_err("Hand executable is missing")?;
    regular_file(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(&path)?.permissions().mode() & 0o111 == 0 {
            bail!("Hand binary is not executable");
        }
    }
    Ok(path)
}
fn parse_status(text: &str, installed: bool) -> ServiceStatus {
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.trim().strip_prefix(name).map(str::trim))
    };
    ServiceStatus {
        installed,
        loaded: true,
        pid: field("pid = ").and_then(|s| s.parse().ok()),
        executable: field("program = ").map(PathBuf::from),
    }
}
pub(crate) async fn status() -> Result<ServiceStatus> {
    refuse_system_service().await?;
    let domain = domain().await?;
    let output = launch(&["print", &format!("{domain}/{LABEL}")]).await?;
    let installed = plist_path()?.exists();
    if output.status.success() {
        return Ok(parse_status(
            &String::from_utf8_lossy(&output.stdout),
            installed,
        ));
    }
    // launchctl uses 113 for an absent service. Other failures must not be
    // mistaken for a stopped owner before an update.
    if output.status.code() != Some(113) {
        bail!("Cannot determine Hand LaunchAgent state");
    }
    let executable = if installed {
        read_plist().await?["ProgramArguments"][0]
            .as_str()
            .map(PathBuf::from)
    } else {
        None
    };
    Ok(ServiceStatus {
        installed,
        loaded: false,
        pid: None,
        executable,
    })
}
pub(crate) async fn stop() -> Result<()> {
    if status().await?.loaded {
        checked(&["bootout", &format!("{}/{LABEL}", domain().await?)]).await?;
    }
    Ok(())
}
pub(crate) async fn start() -> Result<()> {
    let state = status().await?;
    if !state.installed {
        bail!("Install the Hand service first with nanocodex hand install");
    }
    let domain = domain().await?;
    if !state.loaded {
        checked(&[
            "bootstrap",
            &domain,
            plist_path()?
                .to_str()
                .ok_or_else(|| eyre!("Non-UTF8 plist path"))?,
        ])
        .await?;
    } else if state.pid.is_none() {
        checked(&["kickstart", &format!("{domain}/{LABEL}")]).await?;
    }
    Ok(())
}
pub(crate) async fn restart() -> Result<()> {
    stop().await?;
    start().await
}
/// Changes only argv[0]. Caller owns stop/start and rollback ordering.
pub(crate) async fn switch_executable(path: &Path) -> Result<()> {
    refuse_system_service().await?;
    let path = executable(path)?;
    let mut value = read_plist().await?;
    let args = value["ProgramArguments"]
        .as_array_mut()
        .ok_or_else(|| eyre!("Missing Hand program arguments"))?;
    if args.len() < 2 || args[1] != "hand" {
        bail!("LaunchAgent does not directly run nanocodex2 hand");
    }
    args[0] = json!(path);
    value["ExitTimeOut"] = json!(90);
    write_plist(&value)
}
pub(crate) async fn install(binary: Option<PathBuf>, account_file: Option<PathBuf>) -> Result<()> {
    refuse_system_service().await?;
    domain().await?;
    if plist_path()?.exists() {
        bail!(
            "Hand LaunchAgent already exists; use hand restart or update to preserve its configuration"
        );
    }
    let binary =
        executable(&binary.unwrap_or(std::env::current_exe()?.with_file_name("nanocodex2")))?;
    validate_candidate(&binary).await?;
    let home = home()?;
    let account = account_file
        .or_else(|| std::env::var_os("NANOCODEX_ACCOUNT_FILE").map(PathBuf::from))
        .unwrap_or_else(|| home.join(".codex/nanocodex-account.json"));
    if !account.is_absolute() {
        bail!("Account file path must be absolute");
    }
    // Do not open, copy, or serialize account credentials.
    let logs = home.join(".nanocodex/service");
    fs::create_dir_all(&logs)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&logs, fs::Permissions::from_mode(0o700))?;
    }
    let log = logs.join("daemon.log");
    if log.symlink_metadata().is_ok() {
        regular_file(&log)?;
    }
    let mut options = fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&log)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    write_plist(
        &json!({"Label":LABEL,"ProgramArguments":[binary,"hand"],"RunAtLoad":true,"KeepAlive":{"SuccessfulExit":false},"ThrottleInterval":10,"ExitTimeOut":90,"WorkingDirectory":home,"StandardOutPath":log,"StandardErrorPath":log,"EnvironmentVariables":{"HOME":home,"NANOCODEX_ACCOUNT_FILE":account,"PATH":"/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"}}),
    )?;
    let since = SystemTime::now();
    if let Err(error) = async {
        start().await?;
        verify_connected(&binary, since, Duration::from_secs(60)).await?;
        Ok::<(), eyre::Report>(())
    }
    .await
    {
        if let Err(cleanup) = stop().await {
            bail!(
                "Hand installation failed: {error:#}; unloading the new service failed: {cleanup:#}"
            );
        }
        fs::remove_file(plist_path()?)?;
        return Err(error.wrap_err("Hand installation failed; new LaunchAgent removed"));
    }
    Ok(())
}
fn fresh_connected(path: &Path, since: SystemTime) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .is_ok_and(|t| t >= since)
        && fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|v| v["status"] == "connected")
}
/// Require a newly published connected catalog and the expected launchd owner.
pub(crate) async fn verify_connected(
    expected: &Path,
    since: SystemTime,
    timeout: Duration,
) -> Result<ServiceStatus> {
    let expected = executable(expected)?;
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let state = status().await?;
        if state.pid.is_some()
            && state.executable.as_deref() == Some(expected.as_path())
            && let Ok(entries) = fs::read_dir(home()?.join(".nanocodex/hands"))
            && entries.flatten().any(|e| {
                let path = e.path().join("status.json");
                fresh_connected(&path, since)
                    && fs::read(path)
                        .ok()
                        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                        .is_some_and(|v| daemon_matches(&v, state.pid, &expected))
            })
        {
            return Ok(state);
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "Hand did not publish a fresh connected catalog with the expected executable before timeout"
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renders_escaped_paths_and_launch_policy() {
        let text = render(&json!({"ProgramArguments":["/a & <b>/nanocodex2","hand"],"KeepAlive":{"SuccessfulExit":false},"RunAtLoad":true})).unwrap();
        assert!(text.contains("/a &amp; &lt;b&gt;/nanocodex2"));
        assert!(text.contains("<key>SuccessfulExit</key><false/>"));
        assert!(xml("bad\0path").is_err());
    }
    #[test]
    fn launchd_pid_is_required_for_running_state() {
        let s = parse_status(
            "gui/501/com.nanocodex.hand = {\n program = /a path/nanocodex2\n pid = 123\n}",
            true,
        );
        assert_eq!(s.pid, Some(123));
        assert_eq!(s.executable, Some(PathBuf::from("/a path/nanocodex2")));
        assert!(parse_status("state = waiting", true).pid.is_none());
    }
    #[test]
    fn health_rejects_stale_and_disconnected_catalogs() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("status.json");
        fs::write(&path, r#"{"status":"connected"}"#).unwrap();
        assert!(!fresh_connected(
            &path,
            SystemTime::now() + Duration::from_secs(5)
        ));
        assert!(fresh_connected(&path, SystemTime::UNIX_EPOCH));
        fs::write(&path, r#"{"status":"connecting"}"#).unwrap();
        assert!(!fresh_connected(&path, SystemTime::UNIX_EPOCH));
    }
}

fn lock_legacy_launchers(directory: &Path) -> Result<Vec<fs::File>> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut locks = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path().join("launch.lock");
        if path.exists() {
            regular_file(&path)?;
        }
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(nix::libc::O_NOFOLLOW);
        }
        let file = options.open(&path)?;
        fs2::FileExt::try_lock_exclusive(&file)
            .wrap_err("A legacy Hand is starting; retry after it connects")?;
        locks.push(file);
    }
    Ok(locks)
}

fn legacy_publisher(command: &str, uid: &str, install: &Path) -> bool {
    let Some((owner, command)) = command.trim().split_once(char::is_whitespace) else {
        return false;
    };
    let Some(binary) = command.trim().strip_suffix(" __device-hand --daemon") else {
        return false;
    };
    owner == uid
        && Path::new(binary)
            .file_name()
            .is_some_and(|n| n == "nanocodex2")
        && (Path::new(binary).starts_with(install.join("versions"))
            || Path::new(binary) == install.join("current/nanocodex2"))
}

/// Retire only pre-service lease helpers. Old clients may ignore the launch
/// lock and spawn an unmanaged daemon while launchd is switching executables.
/// The interactive CLI itself and protocol-aware helpers remain running.
async fn stop_legacy_helpers() -> Result<()> {
    let home = home()?;
    let domain = domain().await?;
    let uid = domain
        .strip_prefix("gui/")
        .ok_or_else(|| eyre!("invalid user domain"))?;
    let output = Command::new("/bin/ps")
        .args(["-axo", "pid=,uid=,command="])
        .output()
        .await?;
    if !output.status.success() {
        bail!("Cannot inspect legacy Hand helpers");
    }
    let mut checked = std::collections::BTreeMap::new();
    for line in String::from_utf8(output.stdout)?.lines() {
        let Some((pid, command)) = line.trim().split_once(char::is_whitespace) else {
            continue;
        };
        let Some(binary) = legacy_helper_binary(command, uid, &home.join(".nanocodex")) else {
            continue;
        };
        let legacy = if let Some(legacy) = checked.get(&binary) {
            *legacy
        } else {
            let legacy = validate_candidate(&binary).await.is_err();
            checked.insert(binary.clone(), legacy);
            legacy
        };
        if !legacy {
            continue;
        }
        let pid = pid.parse::<u32>()?;
        // Recheck immediately before signalling to avoid acting on a changed PID.
        let current = Command::new("/bin/ps")
            .args(["-p", &pid.to_string(), "-o", "uid=,command="])
            .output()
            .await?;
        if current.status.success()
            && legacy_helper_binary(
                &String::from_utf8(current.stdout)?,
                uid,
                &home.join(".nanocodex"),
            ) == Some(binary)
        {
            let _ = Command::new("/bin/kill")
                .args(["-TERM", &pid.to_string()])
                .status()
                .await?;
        }
    }
    Ok(())
}
fn legacy_helper_binary(command: &str, uid: &str, install: &Path) -> Option<PathBuf> {
    let (owner, command) = command.trim().split_once(char::is_whitespace)?;
    let binary = command
        .trim()
        .strip_suffix(" __device-hand --parent-pipe")?;
    if owner == uid
        && Path::new(binary)
            .file_name()
            .is_some_and(|n| n == "nanocodex2")
        && (Path::new(binary).starts_with(install.join("versions"))
            || Path::new(binary) == install.join("current/nanocodex2"))
    {
        Some(PathBuf::from(binary))
    } else {
        None
    }
}

/// Explicit recovery may retire a legacy publisher that stole the state lock
/// during an older updater's handover. Never signal a CLI, a VM, or another user.
async fn stop_legacy_publishers() -> Result<()> {
    let home = home()?;
    let domain = domain().await?;
    let uid = domain
        .strip_prefix("gui/")
        .ok_or_else(|| eyre!("invalid user domain"))?;
    let directory = home.join(".nanocodex/hands");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let lock = entry.path().join("host.lock");
        if !lock.exists() {
            continue;
        }
        let output = Command::new("/usr/sbin/lsof")
            .arg("-t")
            .arg("--")
            .arg(&lock)
            .output()
            .await?;
        for pid in String::from_utf8(output.stdout)?.split_whitespace() {
            let pid = pid.parse::<u32>()?;
            let inspect = Command::new("/bin/ps")
                .args(["-p", &pid.to_string(), "-o", "uid=,command="])
                .output()
                .await?;
            if !inspect.status.success() {
                continue;
            }
            if !legacy_publisher(
                &String::from_utf8(inspect.stdout)?,
                uid,
                &home.join(".nanocodex"),
            ) {
                bail!("Another process owns the Hand state; refusing to stop it");
            }
            let stopped = Command::new("/bin/kill")
                .args(["-TERM", &pid.to_string()])
                .status()
                .await?;
            if !stopped.success() {
                bail!("Could not stop the legacy Hand publisher");
            }
            let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
            loop {
                let file = fs::OpenOptions::new().read(true).write(true).open(&lock)?;
                if fs2::FileExt::try_lock_exclusive(&file).is_ok() {
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    bail!("Legacy Hand did not release its state lock");
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
    Ok(())
}

/// Prepared service transaction. Backup remains on disk until commit/rollback.
pub(crate) struct ServiceUpdate {
    candidate: PathBuf,
    previous: Vec<u8>,
    was_loaded: bool,
    start_stopped: bool,
    backup: PathBuf,
    // Legacy clients use per-account launch locks before spawning a publisher.
    // Hold them through handover so old clients cannot steal service ownership.
    _legacy_guards: Vec<fs::File>,
}
pub(crate) async fn prepare_update(
    candidate: &Path,
    start_stopped: bool,
) -> Result<Option<ServiceUpdate>> {
    if !cfg!(target_os = "macos") {
        return Ok(None);
    }
    let state = status().await?;
    if !state.installed && !state.loaded {
        return Ok(None);
    }
    if !state.installed {
        bail!("Loaded Hand has no installed LaunchAgent; cannot safely update");
    }
    let candidate = executable(candidate)?;
    validate_candidate(&candidate).await?;
    let legacy_guards = lock_legacy_launchers(&home()?.join(".nanocodex/hands"))?;
    let path = plist_path()?;
    regular_file(&path)?;
    read_plist_path(&path).await?;
    let previous = fs::read(&path)?;
    let backup = path.with_extension("plist.update-backup");
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    let mut snapshot = read_plist_path(&path).await?;
    snapshot["NanocodexUpdateWasLoaded"] = json!(state.loaded);
    write!(
        file,
        "<?xml version=\"1.0\"?><plist version=\"1.0\">{}</plist>",
        render(&snapshot)?
    )?;
    file.as_file().sync_all()?;
    file.persist_noclobber(&backup)
        .wrap_err("A Hand update backup already exists; recover it before updating")?;
    Ok(Some(ServiceUpdate {
        candidate,
        previous,
        was_loaded: state.loaded,
        start_stopped,
        backup,
        _legacy_guards: legacy_guards,
    }))
}
impl ServiceUpdate {
    pub(crate) async fn apply(&mut self) -> Result<()> {
        stop_legacy_helpers().await?;
        stop().await?;
        let since = SystemTime::now();
        switch_executable(&self.candidate).await?;
        if self.was_loaded || self.start_stopped {
            start().await?;
            verify_connected(&self.candidate, since, Duration::from_secs(60)).await?;
        }
        Ok(())
    }
    pub(crate) async fn rollback(&mut self) -> Result<()> {
        stop().await?;
        let since = SystemTime::now();
        let path = plist_path()?;
        let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
        file.write_all(&self.previous)?;
        file.as_file().sync_all()?;
        file.persist(&path)?;
        if self.was_loaded {
            let previous_executable = read_plist().await?["ProgramArguments"][0]
                .as_str()
                .map(PathBuf::from)
                .ok_or_else(|| eyre!("Backup has no Hand executable"))?;
            start().await?;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
            loop {
                let state = status().await?;
                let connected = state.pid.is_some()
                    && state.executable.as_ref() == Some(&previous_executable)
                    && fs::read_dir(home()?.join(".nanocodex/hands")).is_ok_and(|entries| {
                        entries.flatten().any(|e| {
                            let path = e.path().join("status.json");
                            fresh_connected(&path, since)
                                && fs::read(path)
                                    .ok()
                                    .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                                    .is_some_and(|v| {
                                        v.get("daemon").is_none()
                                            || (v["daemon"]["pid"].as_u64()
                                                == state.pid.map(u64::from)
                                                && v["daemon"]["executable"].as_str()
                                                    == previous_executable.to_str())
                                    })
                        })
                    });
                if connected {
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    bail!("Previous Hand service did not reconnect; update backup retained");
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
        self.commit().await
    }
    pub(crate) async fn commit(&mut self) -> Result<()> {
        match fs::remove_file(&self.backup) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

pub(crate) async fn recover() -> Result<()> {
    refuse_system_service().await?;
    let backup = plist_path()?.with_extension("plist.update-backup");
    match fs::symlink_metadata(&backup) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    regular_file(&backup)?;
    let (previous, was_loaded) = restore_snapshot(read_plist_path(&backup).await?)?;
    let guards = lock_legacy_launchers(&home()?.join(".nanocodex/hands"))?;
    stop_legacy_helpers().await?;
    stop().await?;
    if was_loaded {
        stop_legacy_publishers().await?;
    }
    let mut transaction = ServiceUpdate {
        candidate: PathBuf::new(),
        previous,
        was_loaded,
        start_stopped: false,
        backup,
        _legacy_guards: guards,
    };
    transaction.rollback().await
}

const fn competing_owner(plist_exists: bool, system_loaded: bool) -> bool {
    plist_exists || system_loaded
}
fn validate_plist(value: &Value) -> Result<()> {
    let args = value["ProgramArguments"]
        .as_array()
        .ok_or_else(|| eyre!("Missing Hand arguments"))?;
    if value["Label"] != LABEL
        || value.get("Program").is_some()
        || args.len() != 2
        || args[1] != "hand"
        || !args[0].as_str().is_some_and(|s| {
            let p = Path::new(s);
            p.is_absolute() && p.file_name().is_some_and(|n| n == "nanocodex2")
        })
    {
        bail!("LaunchAgent must directly run an absolute nanocodex2 hand executable");
    }
    Ok(())
}
#[cfg(test)]
mod safety_tests {
    use super::*;
    #[test]
    fn rejects_competing_system_owners() {
        assert!(!competing_owner(false, false));
        for (file, loaded) in [(true, false), (false, true), (true, true)] {
            assert!(competing_owner(file, loaded));
        }
    }
    #[test]
    fn refuses_arbitrary_backup_commands() {
        let valid = json!({"Label":LABEL,"ProgramArguments":["/versions/v1/nanocodex2","hand"]});
        assert!(validate_plist(&valid).is_ok());
        for args in [
            json!(["/bin/sh", "hand"]),
            json!(["nanocodex2", "hand"]),
            json!(["/bin/nanocodex2", "other"]),
            json!(["/bin/nanocodex2", "hand", "--other"]),
            json!([null, "hand"]),
        ] {
            let mut v = valid.clone();
            v["ProgramArguments"] = args;
            assert!(validate_plist(&v).is_err());
        }
        let mut v = valid.clone();
        v["Program"] = json!("/bin/sh");
        assert!(validate_plist(&v).is_err());
        let mut v = valid;
        v["Label"] = json!("other");
        assert!(validate_plist(&v).is_err());
    }
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn recovery_parser_validates_backup_without_launching_it() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("backup.plist");
        fs::write(&path, b"invalid plist").unwrap();
        assert!(read_plist_path(&path).await.is_err());
        for (program, accepted) in [("/versions/v1/nanocodex2", true), ("/bin/sh", false)] {
            fs::write(
                &path,
                format!(
                    "<?xml version=\"1.0\"?><plist version=\"1.0\">{}</plist>",
                    render(&json!({"Label":LABEL,"ProgramArguments":[program,"hand"]})).unwrap()
                ),
            )
            .unwrap();
            assert_eq!(read_plist_path(&path).await.is_ok(), accepted);
        }
    }
    #[test]
    fn malformed_status_is_not_connected() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("status.json");
        for content in [
            "{",
            "null",
            "[]",
            r#"{"status":true}"#,
            r#"{"status":"connecting","daemon":{"pid":3}}"#,
        ] {
            fs::write(&path, content).unwrap();
            assert!(!fresh_connected(&path, SystemTime::UNIX_EPOCH));
        }
    }
}

fn daemon_matches(value: &Value, pid: Option<u32>, executable: &Path) -> bool {
    pid.is_some_and(|pid| pid > 0 && value["daemon"]["pid"].as_u64() == Some(u64::from(pid)))
        && executable
            .to_str()
            .is_some_and(|path| value["daemon"]["executable"].as_str() == Some(path))
}
#[cfg(test)]
mod daemon_tests {
    use super::*;
    #[test]
    fn rejects_malformed_or_stale_daemon_metadata() {
        let expected = Path::new("/versions/v2/nanocodex2");
        let good = json!({"status":"connected","daemon":{"pid":123,"executable":expected}});
        assert!(daemon_matches(&good, Some(123), expected));
        assert!(!daemon_matches(&good, Some(124), expected));
        assert!(!daemon_matches(&good, None, expected));
        assert!(!daemon_matches(
            &good,
            Some(123),
            Path::new("/versions/v1/nanocodex2")
        ));
        for daemon in [
            Value::Null,
            json!({}),
            json!({"pid":"123","executable":expected}),
            json!({"pid":123,"executable":null}),
        ] {
            assert!(!daemon_matches(
                &json!({"status":"connected","daemon":daemon}),
                Some(123),
                expected
            ));
        }
    }
}

/// Finalize a committed update after the coordinator verifies its active state.
pub(crate) async fn finish_recovery() -> Result<()> {
    refuse_system_service().await?;
    let backup = plist_path()?.with_extension("plist.update-backup");
    match fs::symlink_metadata(&backup) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    read_plist_path(&backup).await?;
    fs::remove_file(backup)?;
    Ok(())
}

fn restore_snapshot(mut snapshot: Value) -> Result<(Vec<u8>, bool)> {
    validate_plist(&snapshot)?;
    let was_loaded = snapshot
        .as_object_mut()
        .and_then(|v| v.remove("NanocodexUpdateWasLoaded"))
        .and_then(|v| v.as_bool())
        .ok_or_else(|| eyre!("Hand update backup is missing its original loaded state"))?;
    Ok((
        format!(
            "<?xml version=\"1.0\"?><plist version=\"1.0\">{}</plist>",
            render(&snapshot)?
        )
        .into_bytes(),
        was_loaded,
    ))
}
#[cfg(test)]
mod recovery_state_tests {
    use super::*;
    #[test]
    fn recovery_preserves_loaded_and_stopped_state() {
        for loaded in [false, true] {
            let snapshot = json!({"Label":LABEL,"ProgramArguments":["/v1/nanocodex2","hand"],"NanocodexUpdateWasLoaded":loaded});
            let (bytes, restored) = restore_snapshot(snapshot).unwrap();
            assert_eq!(restored, loaded);
            assert!(
                !String::from_utf8(bytes)
                    .unwrap()
                    .contains("NanocodexUpdateWasLoaded")
            );
        }
        assert!(
            restore_snapshot(json!({"Label":LABEL,"ProgramArguments":["/v1/nanocodex2","hand"]}))
                .is_err()
        );
    }
}

/// Probe capability without authenticating or publishing a Hand.
pub(crate) async fn validate_candidate(path: &Path) -> Result<()> {
    let path = executable(path)?;
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        Command::new(&path)
            .args(["__device-hand", "--service-protocol"])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .wrap_err("Hand service protocol probe timed out")??;
    if !output.status.success() || !compatible_protocol(&output.stdout) {
        bail!(
            "Hand candidate does not support service protocol 1; install a matching supported release bundle"
        );
    }
    Ok(())
}
fn compatible_protocol(bytes: &[u8]) -> bool {
    serde_json::from_slice::<Value>(bytes)
        .is_ok_and(|value| value["serviceProtocol"].as_u64() == Some(1))
}
#[cfg(test)]
mod protocol_tests {
    use super::*;
    #[test]
    fn requires_exact_supported_service_protocol() {
        assert!(compatible_protocol(br#"{"serviceProtocol":1}"#));
        for bytes in [
            br#"{"serviceProtocol":2}"#.as_slice(),
            br#"{"serviceProtocol":"1"}"#,
            b"{}",
            b"null",
            b"version 1",
            b"{}\n{}",
        ] {
            assert!(!compatible_protocol(bytes));
        }
    }
}

#[cfg(test)]
mod legacy_tests {
    use super::*;
    #[test]
    fn handover_blocks_legacy_autostart_until_service_is_ready() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("account");
        fs::create_dir(&state).unwrap();
        let guards = lock_legacy_launchers(tmp.path()).unwrap();
        let contender = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(state.join("launch.lock"))
            .unwrap();
        assert!(contender.try_lock().is_err());
        drop(guards);
        assert!(contender.try_lock().is_ok());
    }
    #[test]
    fn legacy_recovery_only_signals_expected_owner_and_publisher_mode() {
        let install = Path::new("/home/test/.nanocodex");
        assert!(legacy_publisher(
            "501 /home/test/.nanocodex/current/nanocodex2 __device-hand --daemon",
            "501",
            install
        ));
        for command in [
            "502 /home/test/.nanocodex/current/nanocodex2 __device-hand --daemon",
            "501 /home/test/.nanocodex/current/nanocodex2 __device-hand --parent-pipe",
            "501 /home/test/.nanocodex/current/nanocodex2 host",
            "501 /other/nanocodex2 __device-hand --daemon",
            "501 /home/test/.nanocodex/current/nanocodex2 __device-hand --daemon --workspace /other",
        ] {
            assert!(!legacy_publisher(command, "501", install));
        }
    }
    #[test]
    fn helper_migration_never_matches_interactive_cli_or_other_user() {
        let install = Path::new("/home/test/.nanocodex");
        assert!(
            legacy_helper_binary(
                "501 /home/test/.nanocodex/current/nanocodex2 __device-hand --parent-pipe",
                "501",
                install
            )
            .is_some()
        );
        for command in [
            "501 /home/test/.nanocodex/current/nanocodex2",
            "501 /home/test/.nanocodex/current/nanocodex2 __device-hand --daemon",
            "502 /home/test/.nanocodex/current/nanocodex2 __device-hand --parent-pipe",
        ] {
            assert!(legacy_helper_binary(command, "501", install).is_none());
        }
    }
}
