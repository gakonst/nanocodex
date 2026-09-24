//! User-owned Windows Hand service managed directly through Task Scheduler.
//!
//! The scheduled task launches the signed Rust worker in the interactive user
//! session. Credentials remain in the normal per-user Nanocodex account store;
//! no secret is copied into the task definition or command line.

use eyre::{Result, WrapErr, bail, eyre};
use serde::{Deserialize, Serialize};
use std::{
    ffi::{OsStr, OsString},
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    time::Duration,
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
    executable: Option<PathBuf>,
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
        (candidate == executable && is_hand).then(|| pid.as_u32())
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
        .wrap_err("Could not start nanocodex2.exe")?;
    if !version.status.success() {
        bail!("nanocodex2.exe could not start");
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

async fn wait_ready(candidate: &Path) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if worker(candidate).is_some() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "Windows started the Hand task, but nanocodex2.exe did not remain running. Check {}",
                data_directory()?.join("hand.log").display()
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    let identity_path = data_directory()?.join("state/identity.json");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let machine = loop {
        if let Ok(metadata) = fs::symlink_metadata(&identity_path)
            && metadata.is_file()
            && metadata.len() <= 64 * 1024
            && let Ok(identity) = fs::read(&identity_path)
            && let Ok(identity) = serde_json::from_slice::<serde_json::Value>(&identity)
            && let Some(machine) = identity["machine_id"].as_str()
        {
            break machine.to_owned();
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "Windows Hand did not create a valid identity. Check {}",
                data_directory()?.join("hand.log").display()
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };

    let (origin, credential) = nanocodex_cli_auth::enrollment_credentials(None)?;
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .build()?;
    while tokio::time::Instant::now() < deadline {
        let hands = account_get(&client, &origin, credential.as_str(), "/v1/account/hands").await;
        let screens = account_get(
            &client,
            &origin,
            credential.as_str(),
            "/v1/account/hands/screens",
        )
        .await;
        if let (Ok(hands), Ok(screens)) = (hands, screens)
            && catalog_ready(&hands, &screens, &machine)
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    bail!(
        "Windows started the Hand, but its Hand and desktop did not appear in the account catalog. Check {}",
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
        None => {
            let sibling = std::env::current_exe()?.with_file_name("nanocodex2.exe");
            if sibling.is_file() {
                sibling
            } else {
                crate::update::active_windows_hand_binary()?.ok_or_else(|| {
                    eyre!("nanocodex2.exe is not installed beside the CLI or in the active bundle")
                })?
            }
        }
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
            return wait_ready(&candidate).await;
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
    if !state.loaded {
        start().await?;
    }
    wait_ready(&executable).await
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

pub(crate) async fn restart() -> Result<()> {
    stop().await?;
    start_and_wait().await
}

pub(crate) struct ServiceUpdate {
    candidate: PathBuf,
    previous: PathBuf,
    was_loaded: bool,
    start_candidate: bool,
}

pub(crate) async fn prepare_update(
    candidate: &Path,
    start_candidate: bool,
) -> Result<Option<ServiceUpdate>> {
    let candidate = validate_candidate(candidate).await?;
    let state = status().await?;
    if !state.installed {
        return Ok(None);
    }
    let previous = state
        .executable
        .ok_or_else(|| eyre!("Windows Hand task has no executable"))?;
    validate_candidate(&previous).await?;
    Ok(Some(ServiceUpdate {
        candidate,
        previous,
        was_loaded: state.loaded,
        start_candidate,
    }))
}

impl ServiceUpdate {
    pub(crate) async fn apply(&mut self) -> Result<()> {
        stop().await?;
        install_task(&self.candidate).await?;
        if self.start_candidate {
            start_and_wait().await?;
        }
        Ok(())
    }

    pub(crate) async fn rollback(&mut self) -> Result<()> {
        stop().await?;
        install_task(&self.previous).await?;
        if self.was_loaded {
            start_and_wait().await?;
        }
        Ok(())
    }

    pub(crate) async fn commit(&mut self) -> Result<()> {
        Ok(())
    }
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
}
