//! User-owned Windows Hand service managed directly through Task Scheduler.
//!
//! The scheduled task launches the signed Rust worker in the interactive user
//! session. Credentials remain in the normal per-user Nanocodex account store;
//! no secret is copied into the task definition or command line.

use eyre::{Result, WrapErr, bail, eyre};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    ffi::{OsStr, OsString},
    fs,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use tokio::process::Command;

const TASK: &str = r"\Nanocodex Hand";
const OWNER: &str = "nanocodex.native-hand.v1";
const LEGACY_SERVICE: &str = "NanocodexHand";

#[derive(Debug, Serialize)]
pub(crate) struct ServiceStatus {
    pub(crate) installed: bool,
    pub(crate) loaded: bool,
    pid: Option<u32>,
    pub(crate) executable: Option<PathBuf>,
    task: &'static str,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TaskRecord {
    owner: String,
    executable: PathBuf,
}

fn supported() -> Result<()> {
    if !cfg!(target_os = "windows") {
        bail!("This local Hand service command requires Windows");
    }
    Ok(())
}

fn home() -> Result<PathBuf> {
    let home = PathBuf::from(
        std::env::var_os("USERPROFILE")
            .filter(|value| !value.is_empty())
            .ok_or_else(|| eyre!("USERPROFILE is not set"))?,
    );
    if !home.is_absolute() {
        bail!("USERPROFILE must be absolute");
    }
    Ok(home)
}

fn data_directory() -> Result<PathBuf> {
    let root = PathBuf::from(
        std::env::var_os("LOCALAPPDATA")
            .filter(|value| !value.is_empty())
            .ok_or_else(|| eyre!("LOCALAPPDATA is not set"))?,
    );
    if !root.is_absolute() {
        bail!("LOCALAPPDATA must be absolute");
    }
    Ok(root.join("Nanocodex").join("Hand"))
}

fn executable(path: &Path) -> Result<PathBuf> {
    let path = fs::canonicalize(path).wrap_err("Hand executable is missing")?;
    if !fs::symlink_metadata(&path)?.is_file() {
        bail!("Expected a regular Hand executable: {}", path.display());
    }
    Ok(path)
}

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

/// Quote one argument according to CommandLineToArgvW's backslash rules.
fn quote_argument(value: &OsStr) -> Result<String> {
    let value = value
        .to_str()
        .ok_or_else(|| eyre!("Windows Hand paths must be valid Unicode"))?;
    if value
        .chars()
        .any(|character| matches!(character, '\0' | '\r' | '\n'))
    {
        bail!("Windows Hand arguments contain an invalid control character");
    }
    if !value.is_empty()
        && !value
            .chars()
            .any(|character| character.is_whitespace() || character == '"')
    {
        return Ok(value.to_owned());
    }
    let mut quoted = String::from("\"");
    let mut backslashes = 0;
    for character in value.chars() {
        if character == '\\' {
            backslashes += 1;
            continue;
        }
        if character == '"' {
            quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
        } else {
            quoted.push_str(&"\\".repeat(backslashes));
        }
        backslashes = 0;
        quoted.push(character);
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    Ok(quoted)
}

fn arguments(workspace: &Path, state: &Path, log: &Path) -> Result<String> {
    [
        OsString::from("hand"),
        OsString::from("--workspace"),
        workspace.as_os_str().to_owned(),
        OsString::from("--state-dir"),
        state.as_os_str().to_owned(),
        OsString::from("--log-format"),
        OsString::from("json"),
        OsString::from("--log-file"),
        log.as_os_str().to_owned(),
    ]
    .iter()
    .map(|argument| quote_argument(argument))
    .collect::<Result<Vec<_>>>()
    .map(|arguments| arguments.join(" "))
}

fn render(
    executable: &Path,
    workspace: &Path,
    state: &Path,
    log: &Path,
    user: &str,
) -> Result<String> {
    let executable = xml(executable
        .to_str()
        .ok_or_else(|| eyre!("Windows Hand executable path must be valid Unicode"))?)?;
    let workspace_text = xml(workspace
        .to_str()
        .ok_or_else(|| eyre!("Windows Hand workspace path must be valid Unicode"))?)?;
    let arguments = xml(&arguments(workspace, state, log)?)?;
    let user = xml(user)?;
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>{OWNER}</Description><URI>{TASK}</URI></RegistrationInfo>
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <AllowStartOnDemand>true</AllowStartOnDemand><Enabled>true</Enabled><Hidden>true</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle><WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit><Priority>7</Priority>
    <RestartOnFailure><Interval>PT1M</Interval><Count>999</Count></RestartOnFailure>
  </Settings>
  <Actions Context="Author"><Exec><Command>{executable}</Command><Arguments>{arguments}</Arguments><WorkingDirectory>{workspace_text}</WorkingDirectory></Exec></Actions>
</Task>
"#
    ))
}

async fn checked(program: &str, arguments: &[&str], operation: &str) -> Result<()> {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .await
        .wrap_err_with(|| format!("Could not {operation}"))?;
    if !output.status.success() {
        bail!(
            "Could not {operation}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

async fn task_definition() -> Result<Option<String>> {
    supported()?;
    let output = Command::new("schtasks.exe")
        .args(["/Query", "/TN", TASK, "/XML"])
        .output()
        .await
        .wrap_err("Could not inspect the Windows Hand task")?;
    if !output.status.success() {
        return Ok(None);
    }
    // schtasks uses the active Windows code page for redirected output. The
    // ownership marker and tags are ASCII; exact Unicode paths come from the
    // private sidecar record.
    let definition = decode_task_xml(&output.stdout);
    if !definition.contains(OWNER) {
        bail!(
            "A different scheduled task already owns {TASK}; remove it before installing the Nanocodex Hand"
        );
    }
    Ok(Some(definition))
}

pub(crate) fn decode_task_xml(bytes: &[u8]) -> String {
    let utf16 = |bytes: &[u8], big_endian: bool| {
        let units = bytes.chunks_exact(2).map(|pair| {
            if big_endian {
                u16::from_be_bytes([pair[0], pair[1]])
            } else {
                u16::from_le_bytes([pair[0], pair[1]])
            }
        });
        String::from_utf16_lossy(&units.collect::<Vec<_>>())
            .trim_start_matches('\u{feff}')
            .to_owned()
    };
    if bytes.starts_with(&[0xff, 0xfe]) || (bytes.len() >= 4 && bytes[1] == 0 && bytes[3] == 0) {
        utf16(bytes, false)
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        utf16(bytes, true)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

fn record_path() -> Result<PathBuf> {
    Ok(data_directory()?.join("task.json"))
}

fn read_record() -> Result<Option<TaskRecord>> {
    let path = record_path()?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => bail!("Windows Hand task record must be a regular file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if metadata.len() > 16 * 1024 {
        bail!("Windows Hand task record is too large");
    }
    let record: TaskRecord = serde_json::from_slice(&fs::read(path)?)?;
    if record.owner != OWNER || !record.executable.is_absolute() {
        bail!("Windows Hand task record is invalid");
    }
    Ok(Some(record))
}

fn write_record(executable: &Path) -> Result<()> {
    let path = record_path()?;
    let parent = path.parent().expect("task record has parent");
    fs::create_dir_all(parent)?;
    if path.symlink_metadata().is_ok() && !fs::symlink_metadata(&path)?.is_file() {
        bail!("Windows Hand task record must be a regular file");
    }
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(
        &mut file,
        &TaskRecord {
            owner: OWNER.to_owned(),
            executable: executable.to_path_buf(),
        },
    )?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

fn command_from_definition(definition: &str) -> Option<PathBuf> {
    let command = definition
        .split_once("<Command>")?
        .1
        .split_once("</Command>")?
        .0;
    let command = command
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&gt;", ">")
        .replace("&lt;", "<")
        .replace("&amp;", "&");
    Some(PathBuf::from(command))
}

fn worker(executable: &Path) -> Option<u32> {
    worker_identity(executable).map(|worker| worker.pid)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct WorkerIdentity {
    pid: u32,
    started: u64,
}

fn worker_identity(executable: &Path) -> Option<WorkerIdentity> {
    let executable = executable.canonicalize().ok()?;
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_exe(UpdateKind::Always)
            .with_cmd(UpdateKind::Always)
            .without_tasks(),
    );
    system.processes().iter().find_map(|(pid, process)| {
        let candidate = process.exe()?.canonicalize().ok()?;
        let is_hand = process
            .cmd()
            .iter()
            .skip(1)
            .any(|argument| argument == OsStr::new("hand"));
        (candidate == executable && is_hand && process.start_time() > 0).then(|| WorkerIdentity {
            pid: pid.as_u32(),
            started: process.start_time(),
        })
    })
}

pub(crate) async fn status() -> Result<ServiceStatus> {
    let Some(definition) = task_definition().await? else {
        return Ok(ServiceStatus {
            installed: false,
            loaded: false,
            pid: None,
            executable: None,
            task: TASK,
        });
    };
    let executable = read_record()?
        .map(|record| record.executable)
        .or_else(|| command_from_definition(&definition))
        .ok_or_else(|| eyre!("Windows Hand task has no executable"))?;
    let pid = worker(&executable);
    Ok(ServiceStatus {
        installed: true,
        loaded: pid.is_some(),
        pid,
        executable: Some(executable),
        task: TASK,
    })
}

#[cfg(not(target_os = "linux"))]
pub(crate) async fn print_status() -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&status().await?)?);
    Ok(())
}

async fn refuse_legacy_service() -> Result<()> {
    let output = Command::new("sc.exe")
        .args(["query", LEGACY_SERVICE])
        .output()
        .await
        .wrap_err("Could not inspect the legacy Windows Hand service")?;
    if output.status.success() {
        bail!(
            "The retired machine-wide Nanocodex Hand service is installed. Uninstall the previous Nanocodex Hand from Windows Settings once, then retry."
        );
    }
    Ok(())
}

pub(crate) fn current_user() -> Result<String> {
    // Rust reads the Unicode environment block directly. That avoids decoding
    // redirected `whoami.exe` output through the machine's legacy code page.
    let name = std::env::var("USERNAME").wrap_err("USERNAME is not set")?;
    let domain = std::env::var("USERDOMAIN").wrap_err("USERDOMAIN is not set")?;
    let user = format!(r"{domain}\{name}");
    if user.is_empty()
        || user.len() > 512
        || user
            .chars()
            .any(|character| character < ' ' || matches!(character, '<' | '>'))
    {
        bail!("Windows returned an invalid current user name");
    }
    Ok(user)
}

async fn validate_candidate(candidate: &Path) -> Result<PathBuf> {
    let candidate = executable(candidate)?;
    let version = Command::new(&candidate)
        .arg("--version")
        .output()
        .await
        .wrap_err("Could not start the Nanocodex Hand executable")?;
    if !version.status.success() {
        bail!("The Nanocodex Hand executable could not start");
    }
    Ok(candidate)
}

async fn install_task(candidate: &Path) -> Result<()> {
    let workspace = home()?;
    let data = data_directory()?;
    let state = data.join("state");
    fs::create_dir_all(&state)?;
    let definition = render(
        candidate,
        &workspace,
        &state,
        &data.join("hand.log"),
        &current_user()?,
    )?;
    let mut task = tempfile::Builder::new().suffix(".xml").tempfile()?;
    // Task Scheduler accepts an explicitly declared UTF-8 document. Avoiding a
    // shell keeps every path and argument data-only.
    task.write_all(definition.as_bytes())?;
    task.as_file().sync_all()?;
    let path = task
        .path()
        .to_str()
        .ok_or_else(|| eyre!("Temporary task path must be valid Unicode"))?;
    checked(
        "schtasks.exe",
        &["/Create", "/TN", TASK, "/XML", path, "/F"],
        "install the Windows Hand task",
    )
    .await?;
    write_record(candidate)
}

fn service_json(path: &Path) -> Option<(serde_json::Value, fs::Metadata)> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > 64 * 1024 {
        return None;
    }
    Some((
        serde_json::from_slice(&fs::read(path).ok()?).ok()?,
        metadata,
    ))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReadinessProof {
    Candidate,
    Restored,
}

fn ready_machine(
    candidate: &Path,
    worker: WorkerIdentity,
    since: SystemTime,
    proof: ReadinessProof,
) -> Option<String> {
    let directory = data_directory().ok()?.join("state");
    let (identity, _) = service_json(&directory.join("identity.json"))?;
    let machine = identity["machine_id"].as_str()?;
    if machine.is_empty() {
        return None;
    }
    // Historical native Hands did not publish status.json. Rollback is already
    // bound to the retained task and executable hash; use their original account
    // catalog contract and keep the exact native process pinned across requests.
    if proof == ReadinessProof::Restored {
        return Some(machine.to_owned());
    }
    let (status, metadata) = service_json(&directory.join("status.json"))?;
    let since = since.max(SystemTime::UNIX_EPOCH + Duration::from_secs(worker.started));
    let since_millis = since
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_millis();
    let executable = Path::new(status["daemon"]["executable"].as_str()?);
    (metadata.modified().ok()? >= since
        && u128::from(status["updated_at_millis"].as_u64()?) >= since_millis
        && status["status"] == "connected"
        && status["screen"]["status"] == "ready"
        && status["screen"]["transport"] == "webrtc"
        && status["machine_id"].as_str() == Some(machine)
        && status["daemon"]["pid"].as_u64() == Some(u64::from(worker.pid))
        && same_executable(executable, candidate))
    .then(|| machine.to_owned())
}

async fn wait_ready(candidate: &Path, since: SystemTime) -> Result<()> {
    wait_publication(candidate, since, ReadinessProof::Candidate).await
}

async fn wait_publication(
    candidate: &Path,
    since: SystemTime,
    proof: ReadinessProof,
) -> Result<()> {
    let candidate = executable(candidate)?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let worker = loop {
        if let Some(worker) = worker_identity(&candidate) {
            break worker;
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "Windows started the Hand task, but the Hand executable did not remain running. Check {}",
                data_directory()?.join("hand.log").display()
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };

    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let (origin, credential) = nanocodex_cli_auth::enrollment_credentials(None)?;
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .build()?;
    while tokio::time::Instant::now() < deadline {
        if worker_identity(&candidate) != Some(worker) {
            bail!(
                "Selected Windows Hand worker exited or changed identity during readiness verification"
            );
        }
        if let Some(machine) = ready_machine(&candidate, worker, since, proof) {
            let catalogs = tokio::time::timeout_at(deadline, async {
                tokio::join!(
                    account_get(&client, &origin, credential.as_str(), "/v1/account/hands"),
                    account_get(
                        &client,
                        &origin,
                        credential.as_str(),
                        "/v1/account/hands/screens"
                    )
                )
            })
            .await;
            // Catalog IDs can survive a disconnected publisher. Re-read the
            // fresh local proof and exact process after the network requests.
            if let Ok((Ok(hands), Ok(screens))) = catalogs
                && catalog_ready(&hands, &screens, &machine)
                && ready_machine(&candidate, worker, since, proof).as_deref()
                    == Some(machine.as_str())
                && worker_identity(&candidate) == Some(worker)
            {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let required = match proof {
        ReadinessProof::Candidate => "fresh connected and WebRTC screen-ready status",
        ReadinessProof::Restored => "its retained Hand and desktop",
    };
    bail!(
        "Windows Hand did not publish {required} from the selected worker and appear in the account catalog. Check {}",
        data_directory()?.join("hand.log").display()
    )
}

async fn account_get(
    client: &reqwest::Client,
    origin: &str,
    credential: &str,
    path: &str,
) -> Result<serde_json::Value> {
    Ok(client
        .get(format!("{origin}{path}"))
        .bearer_auth(credential)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

fn catalog_ready(hands: &serde_json::Value, screens: &serde_json::Value, machine: &str) -> bool {
    let hand = hands["data"]
        .as_array()
        .is_some_and(|hands| hands.iter().any(|hand| hand["id"] == machine));
    let screen = screens["surfaces"]
        .as_array()
        .is_some_and(|screens| screens.iter().any(|screen| screen["machine_id"] == machine));
    hand && screen
}

pub(crate) async fn ensure(candidate: Option<PathBuf>) -> Result<()> {
    supported()?;
    refuse_legacy_service().await?;
    let candidate = match candidate {
        Some(candidate) => candidate,
        // This one executable is the Hand; keep an identical nanocodex2.exe
        // beside it as the task's recorded name when present.
        None => crate::hand_executable::hand_binary()?,
    };
    let candidate = validate_candidate(&candidate).await?;
    if let Some(existing) = task_definition().await? {
        let selected = read_record()?
            .map(|record| record.executable)
            .or_else(|| command_from_definition(&existing))
            .ok_or_else(|| eyre!("Windows Hand task has no executable"))?;
        if executable(&selected).ok().as_deref() == Some(candidate.as_path())
            && worker(&candidate).is_some()
        {
            return wait_ready(&candidate, SystemTime::UNIX_EPOCH).await;
        }
        stop().await?;
    }
    install_task(&candidate).await?;
    start_and_wait().await
}

pub(crate) async fn start() -> Result<()> {
    if task_definition().await?.is_none() {
        bail!("Windows Hand is not installed; run `nanocodex hand install`");
    }
    checked(
        "schtasks.exe",
        &["/Run", "/TN", TASK],
        "start the Windows Hand task",
    )
    .await
}

pub(crate) async fn start_and_wait() -> Result<()> {
    let state = status().await?;
    let executable = state
        .executable
        .ok_or_else(|| eyre!("Windows Hand is not installed; run `nanocodex hand install`"))?;
    let since = if state.loaded {
        SystemTime::UNIX_EPOCH
    } else {
        SystemTime::now()
    };
    if !state.loaded {
        start().await?;
    }
    wait_ready(&executable, since).await
}

pub(crate) async fn stop() -> Result<()> {
    let state = status().await?;
    if !state.installed || !state.loaded {
        return Ok(());
    }
    checked(
        "schtasks.exe",
        &["/End", "/TN", TASK],
        "stop the Windows Hand task",
    )
    .await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while state.executable.as_deref().and_then(worker).is_some() {
        if tokio::time::Instant::now() >= deadline {
            bail!("Windows Hand did not stop before timeout");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub(crate) async fn restart() -> Result<()> {
    stop().await?;
    start_and_wait().await
}

// The stable sibling can be overwritten by sync_windows_entrypoints after the
// task has switched. A path alone is therefore never rollback evidence.
const UPDATE_BACKUP: &str = "task.update-backup";

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoveryRecord {
    previous: PathBuf,
    candidate: PathBuf,
    previous_sha256: String,
    candidate_sha256: String,
    definition_sha256: String,
    record_sha256: Option<String>,
    was_loaded: bool,
    start_candidate: bool,
}

impl RecoveryRecord {
    pub(crate) fn candidate(&self) -> &Path {
        &self.candidate
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct UpdateSnapshot {
    owner: String,
    recovery: RecoveryRecord,
    definition: String,
    // Preserve absence as well as the exact original sidecar bytes. No account
    // file is read, copied, or written by this transaction.
    record: Option<Vec<u8>>,
}

pub(crate) struct ServiceUpdate {
    recovery: RecoveryRecord,
    backup: PathBuf,
}

fn backup_path() -> Result<PathBuf> {
    Ok(data_directory()?.join(UPDATE_BACKUP))
}

fn regular_file(path: &Path) -> Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path).wrap_err_with(|| {
        format!(
            "Required Windows Hand update evidence is missing: {}",
            path.display()
        )
    })?;
    if !metadata.is_file() {
        bail!(
            "Windows Hand update evidence must be a regular file: {}",
            path.display()
        );
    }
    Ok(metadata)
}

fn sha256_file(path: &Path) -> Result<String> {
    regular_file(path)?;
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hex::encode(hash.finalize()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn same_executable(left: &Path, right: &Path) -> bool {
    left == right
        || left
            .canonicalize()
            .ok()
            .is_some_and(|left| right.canonicalize().ok().as_deref() == Some(left.as_path()))
}

fn read_snapshot(backup: &Path) -> Result<UpdateSnapshot> {
    if !fs::symlink_metadata(backup)
        .wrap_err("Windows Hand update backup is missing; refusing to guess previous task state")?
        .is_dir()
    {
        bail!("Windows Hand update backup must be a regular directory");
    }
    let manifest = backup.join("snapshot.json");
    if regular_file(&manifest)?.len() > 1024 * 1024 {
        bail!("Windows Hand update snapshot is too large");
    }
    let snapshot: UpdateSnapshot = serde_json::from_slice(&fs::read(manifest)?)?;
    let record = &snapshot.recovery;
    if snapshot.owner != OWNER
        || !record.previous.is_absolute()
        || !record.candidate.is_absolute()
        || !snapshot.definition.contains(OWNER)
        || !command_from_definition(&snapshot.definition)
            .is_some_and(|command| same_executable(&command, &record.previous))
        || sha256_bytes(snapshot.definition.as_bytes()) != record.definition_sha256
        || snapshot.record.as_deref().map(sha256_bytes) != record.record_sha256
        || snapshot
            .record
            .as_ref()
            .is_some_and(|bytes| bytes.len() > 16 * 1024)
    {
        bail!("Windows Hand update snapshot is invalid");
    }
    if let Some(bytes) = &snapshot.record {
        let sidecar: TaskRecord = serde_json::from_slice(bytes)?;
        if sidecar.owner != OWNER || sidecar.executable != record.previous {
            bail!("Windows Hand update snapshot sidecar does not match the previous executable");
        }
    }
    if sha256_file(&backup.join("previous.exe"))? != record.previous_sha256 {
        bail!("Windows Hand update backup executable failed checksum verification");
    }
    Ok(snapshot)
}

fn save_snapshot(backup: &Path, snapshot: &UpdateSnapshot) -> Result<()> {
    if fs::symlink_metadata(backup).is_ok() {
        bail!("A Windows Hand update backup already exists; run nanocodex hand recover first");
    }
    let parent = backup
        .parent()
        .ok_or_else(|| eyre!("Update backup has no parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = tempfile::Builder::new()
        .prefix("task-update-")
        .tempdir_in(parent)?;
    let previous = temporary.path().join("previous.exe");
    let mut retained = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&previous)?;
    std::io::copy(
        &mut fs::File::open(&snapshot.recovery.previous)?,
        &mut retained,
    )?;
    retained.set_permissions(fs::metadata(&snapshot.recovery.previous)?.permissions())?;
    retained.sync_all()?;
    drop(retained);
    if sha256_file(&previous)? != snapshot.recovery.previous_sha256 {
        bail!("Windows Hand previous executable changed during update preparation");
    }
    let mut manifest = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temporary.path().join("snapshot.json"))?;
    serde_json::to_writer(&mut manifest, snapshot)?;
    manifest.write_all(b"\n")?;
    manifest.sync_all()?;
    drop(manifest);
    // Publish only a complete snapshot. It is never updated or re-used for a
    // different candidate, and survives process exit/crash until commit.
    fs::rename(temporary.path(), backup)?;
    Ok(())
}

fn replace_file(path: &Path, source: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => bail!("Windows Hand executable must be a regular file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    regular_file(source)?;
    let parent = path
        .parent()
        .ok_or_else(|| eyre!("Hand executable has no parent"))?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    std::io::copy(&mut fs::File::open(source)?, &mut file)?;
    file.as_file()
        .set_permissions(fs::metadata(source)?.permissions())?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

fn restore_record(path: &Path, bytes: Option<&[u8]>) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => bail!("Windows Hand task record must be a regular file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    if let Some(bytes) = bytes {
        let parent = path
            .parent()
            .ok_or_else(|| eyre!("Task record has no parent"))?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(bytes)?;
        file.as_file().sync_all()?;
        file.persist(path)?;
    } else {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn definition_with_executable(definition: &str, candidate: &Path) -> Result<String> {
    let (before, command) = definition
        .split_once("<Command>")
        .ok_or_else(|| eyre!("Windows Hand task has no executable"))?;
    let (_, after) = command
        .split_once("</Command>")
        .ok_or_else(|| eyre!("Windows Hand task has an invalid executable"))?;
    if after.contains("<Command>") {
        bail!("Windows Hand task contains more than one executable");
    }
    let candidate = xml(candidate
        .to_str()
        .ok_or_else(|| eyre!("Windows Hand executable path must be valid Unicode"))?)?;
    Ok(format!("{before}<Command>{candidate}</Command>{after}"))
}

async fn stop_update_task(record: &RecoveryRecord) -> Result<()> {
    let Some(definition) = task_definition().await? else {
        return Ok(());
    };
    // Do not rely on a sidecar that may not yet have been written after /Create.
    let selected = command_from_definition(&definition)
        .ok_or_else(|| eyre!("Windows Hand task has no executable"))?;
    if !same_executable(&selected, &record.previous)
        && !same_executable(&selected, &record.candidate)
    {
        bail!("Windows Hand task changed outside the update; refusing to stop it");
    }
    if worker(&selected).is_none() {
        return Ok(());
    }
    checked(
        "schtasks.exe",
        &["/End", "/TN", TASK],
        "stop the Windows Hand update task",
    )
    .await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while worker(&selected).is_some() {
        if tokio::time::Instant::now() >= deadline {
            bail!("Windows Hand did not stop before update timeout");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

async fn restore_definition(definition: &str) -> Result<()> {
    // Query output may declare UTF-16. Preserve the whole task configuration,
    // only normalizing the declaration to the encoding used by this import.
    let body = definition.trim_start_matches('\u{feff}');
    let body = if body.starts_with("<?xml") {
        body.split_once("?>")
            .ok_or_else(|| eyre!("Invalid Windows task XML declaration"))?
            .1
    } else {
        body
    };
    let mut task = tempfile::Builder::new().suffix(".xml").tempfile()?;
    task.write_all(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>")?;
    task.write_all(body.as_bytes())?;
    task.as_file().sync_all()?;
    let path = task
        .path()
        .to_str()
        .ok_or_else(|| eyre!("Temporary task path must be valid Unicode"))?;
    checked(
        "schtasks.exe",
        &["/Create", "/TN", TASK, "/XML", path, "/F"],
        "restore the previous Windows Hand task",
    )
    .await
}

pub(crate) async fn prepare_update(
    candidate: &Path,
    start_candidate: bool,
) -> Result<Option<ServiceUpdate>> {
    let candidate = validate_candidate(candidate).await?;
    let Some(definition) = task_definition().await? else {
        return Ok(None);
    };
    let sidecar = read_record()?;
    let previous = sidecar
        .as_ref()
        .map(|record| record.executable.clone())
        .or_else(|| command_from_definition(&definition))
        .ok_or_else(|| eyre!("Windows Hand task has no executable"))?;
    if !previous.is_absolute() {
        bail!("Windows Hand task executable must be absolute");
    }
    let selected = validate_candidate(&previous).await?;
    let task_command = command_from_definition(&definition)
        .ok_or_else(|| eyre!("Windows Hand task has no executable"))?;
    if executable(&task_command)? != selected {
        bail!("Windows Hand task command and private record disagree; inspect before updating");
    }
    let record = if sidecar.is_some() {
        Some(fs::read(record_path()?)?)
    } else {
        None
    };
    let recovery = RecoveryRecord {
        previous,
        candidate: candidate.clone(),
        previous_sha256: sha256_file(&selected)?,
        candidate_sha256: sha256_file(&candidate)?,
        definition_sha256: sha256_bytes(definition.as_bytes()),
        record_sha256: record.as_deref().map(sha256_bytes),
        was_loaded: worker(&selected).is_some(),
        start_candidate,
    };
    let backup = backup_path()?;
    save_snapshot(
        &backup,
        &UpdateSnapshot {
            owner: OWNER.to_owned(),
            recovery: recovery.clone(),
            definition,
            record,
        },
    )?;
    Ok(Some(ServiceUpdate { recovery, backup }))
}

impl ServiceUpdate {
    pub(crate) fn recovery_record(&self) -> &RecoveryRecord {
        &self.recovery
    }

    pub(crate) async fn apply(&mut self) -> Result<()> {
        let snapshot = read_snapshot(&self.backup)?;
        if snapshot.recovery != self.recovery {
            bail!("Windows Hand update backup belongs to a different transaction");
        }
        if sha256_file(&self.recovery.candidate)? != self.recovery.candidate_sha256 {
            bail!("Windows Hand candidate changed after update preparation");
        }
        stop_update_task(&self.recovery).await?;
        restore_definition(&definition_with_executable(
            &snapshot.definition,
            &self.recovery.candidate,
        )?)
        .await?;
        write_record(&self.recovery.candidate)?;
        if self.recovery.start_candidate {
            start_and_wait().await?;
        } else {
            stop_update_task(&self.recovery).await?;
        }
        Ok(())
    }

    pub(crate) async fn rollback(&mut self) -> Result<()> {
        // Validate all backup evidence before any service action. A missing or
        // corrupt backup must not install a guessed task or start a stopped one.
        let snapshot = read_snapshot(&self.backup)?;
        if snapshot.recovery != self.recovery {
            bail!("Windows Hand update backup belongs to a different transaction");
        }
        stop_update_task(&self.recovery).await?;
        replace_file(&self.recovery.previous, &self.backup.join("previous.exe"))?;
        restore_definition(&snapshot.definition).await?;
        restore_record(&record_path()?, snapshot.record.as_deref())?;
        if self.recovery.was_loaded {
            let since = SystemTime::now();
            start().await?;
            wait_publication(&self.recovery.previous, since, ReadinessProof::Restored).await?;
        } else {
            stop_update_task(&self.recovery).await?;
        }
        // Retain evidence until the outer CLI journal has committed/recovered.
        Ok(())
    }

    pub(crate) async fn commit(&mut self) -> Result<()> {
        finish_recovery(&self.recovery).await
    }
}

/// Also handles the crash between backup publication and adding windowsHand to
/// the outer journal. Missing evidence is not permission to re-render a task.
pub(crate) fn pending_recovery_record() -> Result<Option<RecoveryRecord>> {
    let backup = backup_path()?;
    match fs::symlink_metadata(&backup) {
        Ok(_) => Ok(Some(read_snapshot(&backup)?.recovery)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

async fn finish_recovery(record: &RecoveryRecord) -> Result<()> {
    let state = status().await?;
    let selected = state.executable.as_deref().map(executable).transpose()?;
    let definition = task_definition()
        .await?
        .ok_or_else(|| eyre!("Committed Windows Hand task is missing"))?;
    let command = command_from_definition(&definition)
        .ok_or_else(|| eyre!("Committed Windows Hand task has no executable"))?;
    if !same_executable(&command, &record.candidate)
        || !state.installed
        || selected.as_deref() != Some(record.candidate.as_path())
        || sha256_file(&record.candidate)? != record.candidate_sha256
        || state.loaded != record.start_candidate
    {
        bail!(
            "Committed Windows Hand no longer matches the selected update/state; inspect before recovery"
        );
    }
    if record.start_candidate {
        // Never start the task here: a committed stopped task stays stopped, and
        // a missing running task is an ambiguous state, not a repair request.
        wait_ready(&record.candidate, SystemTime::UNIX_EPOCH).await?;
    }
    let backup = backup_path()?;
    match fs::symlink_metadata(&backup) {
        Ok(_) => {
            if read_snapshot(&backup)?.recovery != *record {
                bail!("Windows Hand update backup belongs to a different transaction");
            }
            fs::remove_dir_all(backup)?;
        }
        // Commit may have removed its backup just before the outer journal was
        // removed. The recorded candidate hash and start state still prove it.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

async fn verify_restored(record: &RecoveryRecord) -> Result<()> {
    let state = status().await?;
    let definition = task_definition()
        .await?
        .ok_or_else(|| eyre!("Restored Windows Hand task is missing"))?;
    let command = command_from_definition(&definition)
        .ok_or_else(|| eyre!("Restored Windows Hand task has no executable"))?;
    let sidecar = match fs::read(record_path()?) {
        Ok(bytes) => Some(sha256_bytes(&bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if !state.installed
        || state.loaded != record.was_loaded
        || !same_executable(&command, &record.previous)
        || sha256_bytes(definition.as_bytes()) != record.definition_sha256
        || sidecar != record.record_sha256
        || sha256_file(&record.previous)? != record.previous_sha256
    {
        bail!(
            "Windows Hand rollback evidence is missing and the exact previous task/bytes/state cannot be verified"
        );
    }
    if record.was_loaded {
        wait_publication(
            &record.previous,
            SystemTime::UNIX_EPOCH,
            ReadinessProof::Restored,
        )
        .await?;
    }
    Ok(())
}

pub(crate) async fn recover(record: &RecoveryRecord, committed: bool) -> Result<()> {
    supported()?;
    if committed {
        return finish_recovery(record).await;
    }
    let backup = backup_path()?;
    if !backup.try_exists()? {
        // Cleanup may have completed just before the outer CLI journal was
        // removed. Verify the exact old task, sidecar, bytes and loaded state;
        // do not synthesize a task or blindly restart it from a missing backup.
        return verify_restored(record).await;
    }
    let mut update = ServiceUpdate {
        recovery: record.clone(),
        backup,
    };
    update.rollback().await
}

/// Called only after old CLI entrypoints have also been restored. Separating
/// cleanup from rollback keeps evidence if the coordinator's CLI recovery fails.
pub(crate) async fn finish_rollback(record: &RecoveryRecord) -> Result<()> {
    verify_restored(record).await?;
    let backup = backup_path()?;
    if !backup.try_exists()? {
        return Ok(());
    }
    if read_snapshot(&backup)?.recovery != *record {
        bail!("Windows Hand update backup belongs to a different transaction");
    }
    if sha256_file(&record.previous)? != record.previous_sha256 {
        bail!("Restored Windows Hand executable no longer matches the update backup");
    }
    fs::remove_dir_all(backup)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_windows_arguments_without_shell_interpolation() {
        assert_eq!(quote_argument(OsStr::new("plain")).unwrap(), "plain");
        assert_eq!(quote_argument(OsStr::new("a b")).unwrap(), "\"a b\"");
        assert_eq!(quote_argument(OsStr::new("")).unwrap(), "\"\"");
        assert_eq!(
            quote_argument(OsStr::new("C:\\a b\\")).unwrap(),
            "\"C:\\a b\\\\\""
        );
        assert_eq!(
            quote_argument(OsStr::new("a\\\"b")).unwrap(),
            "\"a\\\\\\\"b\""
        );
    }

    #[test]
    fn task_runs_the_worker_directly_and_carries_no_secret() {
        let rendered = render(
            Path::new(r"C:\Program Files\Nanocodex\nanocodex2.exe"),
            Path::new(r"C:\Users\A & B"),
            Path::new(r"C:\Users\A & B\state"),
            Path::new(r"C:\Users\A & B\hand.log"),
            r"DESKTOP\alice",
        )
        .unwrap();
        assert!(rendered.contains(OWNER));
        assert!(rendered.contains("nanocodex2.exe</Command>"));
        assert!(rendered.contains("A &amp; B"));
        assert!(rendered.contains("<RestartOnFailure>"));
        assert!(!rendered.contains("powershell"));
        assert!(!rendered.contains("account"));
        assert_eq!(
            command_from_definition(&rendered).unwrap(),
            PathBuf::from(r"C:\Program Files\Nanocodex\nanocodex2.exe")
        );
    }

    #[test]
    fn readiness_requires_the_same_hand_and_screen_identity() {
        let hands = serde_json::json!({"data":[{"id":"machine"}]});
        let screens = serde_json::json!({"surfaces":[{"machine_id":"machine"}]});
        assert!(catalog_ready(&hands, &screens, "machine"));
        assert!(!catalog_ready(&hands, &screens, "other"));
        assert!(!catalog_ready(
            &hands,
            &serde_json::json!({"surfaces":[]}),
            "machine"
        ));
    }

    #[test]
    fn task_xml_decoder_accepts_windows_utf16_and_utf8_output() {
        let expected = "<Description>nanocodex.native-hand.v1</Description>";
        let utf16 = expected
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        assert_eq!(decode_task_xml(&utf16), expected);
        assert_eq!(decode_task_xml(expected.as_bytes()), expected);
    }

    fn snapshot_fixture(directory: &Path, was_loaded: bool) -> UpdateSnapshot {
        let previous = directory.join("previous & worker.exe");
        let candidate = directory.join("candidate.exe");
        fs::write(&previous, b"immutable old worker").unwrap();
        fs::write(&candidate, b"new worker").unwrap();
        let definition = render(&previous, directory, directory, directory, "fixture\\user")
            .unwrap()
            .replace("<Priority>7</Priority>", "<Priority>11</Priority>");
        let mut record = serde_json::to_vec(&TaskRecord {
            owner: OWNER.to_owned(),
            executable: previous.clone(),
        })
        .unwrap();
        record.extend_from_slice(b"\n");
        UpdateSnapshot {
            owner: OWNER.to_owned(),
            recovery: RecoveryRecord {
                previous: previous.clone(),
                candidate: candidate.clone(),
                previous_sha256: sha256_file(&previous).unwrap(),
                candidate_sha256: sha256_file(&candidate).unwrap(),
                definition_sha256: sha256_bytes(definition.as_bytes()),
                record_sha256: Some(sha256_bytes(&record)),
                was_loaded,
                start_candidate: true,
            },
            definition,
            record: Some(record),
        }
    }

    #[test]
    fn snapshot_restores_immutable_bytes_after_stable_sibling_overwrite() {
        let directory = tempfile::tempdir().unwrap();
        let snapshot = snapshot_fixture(directory.path(), true);
        let backup = directory.path().join(UPDATE_BACKUP);
        save_snapshot(&backup, &snapshot).unwrap();
        fs::write(
            &snapshot.recovery.previous,
            b"new worker overwrote stable sibling",
        )
        .unwrap();
        let retained = read_snapshot(&backup).unwrap();
        assert_eq!(retained.recovery, snapshot.recovery);
        assert_eq!(retained.definition, snapshot.definition);
        assert_eq!(retained.record, snapshot.record);
        assert_eq!(
            fs::read(backup.join("previous.exe")).unwrap(),
            b"immutable old worker"
        );
        replace_file(&retained.recovery.previous, &backup.join("previous.exe")).unwrap();
        assert_eq!(
            fs::read(&snapshot.recovery.previous).unwrap(),
            b"immutable old worker"
        );
        assert_eq!(
            sha256_file(&snapshot.recovery.previous).unwrap(),
            snapshot.recovery.previous_sha256
        );
        // Rollback evidence remains available for an outer CLI recovery failure.
        assert!(backup.exists());
    }

    #[test]
    fn snapshot_preserves_loaded_and_stopped_state_without_guessing() {
        for was_loaded in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let mut snapshot = snapshot_fixture(directory.path(), was_loaded);
            snapshot.recovery.start_candidate = false;
            let backup = directory.path().join(UPDATE_BACKUP);
            save_snapshot(&backup, &snapshot).unwrap();
            let retained = read_snapshot(&backup).unwrap();
            assert_eq!(retained.recovery.was_loaded, was_loaded);
            assert!(!retained.recovery.start_candidate);
            let journal = serde_json::to_vec(&retained.recovery).unwrap();
            assert_eq!(
                serde_json::from_slice::<RecoveryRecord>(&journal).unwrap(),
                snapshot.recovery
            );
        }
    }

    #[test]
    fn refuses_to_overwrite_existing_transaction_backup() {
        let directory = tempfile::tempdir().unwrap();
        let snapshot = snapshot_fixture(directory.path(), false);
        let backup = directory.path().join(UPDATE_BACKUP);
        save_snapshot(&backup, &snapshot).unwrap();
        let manifest = fs::read(backup.join("snapshot.json")).unwrap();
        assert!(
            save_snapshot(&backup, &snapshot)
                .unwrap_err()
                .to_string()
                .contains("already exists")
        );
        assert_eq!(fs::read(backup.join("snapshot.json")).unwrap(), manifest);
    }

    #[tokio::test]
    async fn missing_or_corrupt_backup_fails_before_task_scheduler_actions() {
        let directory = tempfile::tempdir().unwrap();
        let snapshot = snapshot_fixture(directory.path(), false);
        let backup = directory.path().join(UPDATE_BACKUP);
        let mut update = ServiceUpdate {
            recovery: snapshot.recovery.clone(),
            backup: backup.clone(),
        };
        assert!(
            update
                .rollback()
                .await
                .unwrap_err()
                .to_string()
                .contains("backup is missing")
        );
        save_snapshot(&backup, &snapshot).unwrap();
        fs::write(backup.join("previous.exe"), b"corrupt backup").unwrap();
        assert!(
            update
                .rollback()
                .await
                .unwrap_err()
                .to_string()
                .contains("checksum verification")
        );
        assert_eq!(
            fs::read(&snapshot.recovery.previous).unwrap(),
            b"immutable old worker"
        );
        assert!(backup.exists());
    }

    #[tokio::test]
    async fn changed_candidate_fails_before_stopping_the_previous_task() {
        let directory = tempfile::tempdir().unwrap();
        let snapshot = snapshot_fixture(directory.path(), true);
        let backup = directory.path().join(UPDATE_BACKUP);
        save_snapshot(&backup, &snapshot).unwrap();
        fs::write(&snapshot.recovery.candidate, b"changed candidate").unwrap();
        let mut update = ServiceUpdate {
            recovery: snapshot.recovery.clone(),
            backup: backup.clone(),
        };
        assert!(
            update
                .apply()
                .await
                .unwrap_err()
                .to_string()
                .contains("candidate changed")
        );
        assert_eq!(
            fs::read(&snapshot.recovery.previous).unwrap(),
            b"immutable old worker"
        );
        assert!(backup.exists());
    }

    #[test]
    fn preserves_exact_sidecar_bytes_and_original_absence() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("task.json");
        let previous = b"{\n  \"owner\": \"fixture\"\n}\n";
        fs::write(&path, b"new record").unwrap();
        restore_record(&path, Some(previous)).unwrap();
        assert_eq!(fs::read(&path).unwrap(), previous);
        restore_record(&path, None).unwrap();
        assert!(!path.exists());
        restore_record(&path, None).unwrap();
    }

    #[test]
    fn candidate_definition_changes_only_executable_and_preserves_settings() {
        let directory = tempfile::tempdir().unwrap();
        let snapshot = snapshot_fixture(directory.path(), false);
        let candidate = directory.path().join("new & worker.exe");
        let switched = definition_with_executable(&snapshot.definition, &candidate).unwrap();
        assert_eq!(command_from_definition(&switched).unwrap(), candidate);
        assert!(switched.contains("<Priority>11</Priority>"));
        assert!(switched.contains("new &amp; worker.exe"));
        assert_eq!(
            definition_with_executable(&switched, &snapshot.recovery.previous).unwrap(),
            snapshot.definition
        );
        assert!(
            definition_with_executable("<Command>a</Command><Command>b</Command>", &candidate)
                .is_err()
        );
    }

    #[test]
    fn modified_task_configuration_and_sidecar_fail_snapshot_verification() {
        let directory = tempfile::tempdir().unwrap();
        let mut snapshot = snapshot_fixture(directory.path(), false);
        let backup = directory.path().join(UPDATE_BACKUP);
        snapshot.definition = snapshot
            .definition
            .replace("<Priority>11</Priority>", "<Priority>8</Priority>");
        save_snapshot(&backup, &snapshot).unwrap();
        assert!(
            read_snapshot(&backup)
                .unwrap_err()
                .to_string()
                .contains("snapshot is invalid")
        );
        fs::remove_dir_all(&backup).unwrap();
        let mut snapshot = snapshot_fixture(directory.path(), false);
        snapshot.record = None;
        save_snapshot(&backup, &snapshot).unwrap();
        assert!(
            read_snapshot(&backup)
                .unwrap_err()
                .to_string()
                .contains("snapshot is invalid")
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_backup_evidence_is_rejected() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let snapshot = snapshot_fixture(directory.path(), false);
        let backup = directory.path().join(UPDATE_BACKUP);
        save_snapshot(&backup, &snapshot).unwrap();
        let link = directory.path().join("linked-backup");
        symlink(&backup, &link).unwrap();
        assert!(read_snapshot(&link).is_err());
        let original = backup.join("previous.exe");
        fs::remove_file(&original).unwrap();
        symlink(&snapshot.recovery.previous, &original).unwrap();
        assert!(read_snapshot(&backup).is_err());
    }
}
