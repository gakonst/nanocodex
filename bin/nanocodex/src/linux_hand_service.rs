//! The controller updates an independently owned systemd Hand. It never starts
//! a foreground Hand or changes its user, account, boot unit, factory or guests.
use std::{
    fs,
    io::IsTerminal as _,
    path::{Path, PathBuf},
    time::Duration,
};

use eyre::{Context, Result, bail, eyre};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::{io::AsyncWriteExt as _, process::Command};

const UNIT: &str = "nanocodex-hand.service";
const SYSTEMCTL: &str = "/usr/bin/systemctl";
const ROOT: &str = "/opt/nanocodex";

#[derive(Debug, Serialize)]
pub(crate) struct ServiceStatus {
    pub installed: bool,
    pub loaded: bool,
    pub pid: Option<u32>,
    pub executable: Option<PathBuf>,
    pub unit: &'static str,
    pub load_state: String,
    pub active_state: String,
}

pub(crate) async fn status() -> Result<ServiceStatus> {
    if !Path::new("/run/systemd/system").is_dir() {
        return Ok(ServiceStatus {
            installed: false,
            loaded: false,
            pid: None,
            executable: None,
            unit: UNIT,
            load_state: "not-found".into(),
            active_state: "inactive".into(),
        });
    }
    let output = Command::new(SYSTEMCTL)
        .args([
            "show",
            UNIT,
            "--no-pager",
            "--property=LoadState,ActiveState,MainPID",
        ])
        .env("LC_ALL", "C")
        .kill_on_drop(true)
        .output()
        .await
        .wrap_err("could not inspect the Linux Hand service")?;
    if !output.status.success() {
        bail!("could not inspect the Linux Hand service");
    }
    let text = String::from_utf8(output.stdout).wrap_err("invalid systemd status")?;
    let property = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key))
            .unwrap_or("")
    };
    let load = property("LoadState=").to_owned();
    let active = property("ActiveState=").to_owned();
    let pid = property("MainPID=").parse::<u32>().ok().filter(|p| *p != 0);
    if !matches!(load.as_str(), "loaded" | "not-found" | "masked") {
        bail!("Linux Hand has an unsupported systemd load state; repair it before updating");
    }
    if load == "masked" {
        bail!("Linux Hand is masked; refusing to bypass administrator policy");
    }
    Ok(ServiceStatus {
        installed: load == "loaded",
        loaded: pid.is_some() || active == "active",
        pid,
        executable: (load == "loaded").then(|| PathBuf::from(ROOT).join("current/nanocodex2")),
        unit: UNIT,
        load_state: load,
        active_state: active,
    })
}

pub(crate) async fn print_status() -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&status().await?)?);
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoveryRecord {
    pub transaction: String,
    pub candidate_sha256: String,
    // Before preparation this is the selected, verified companion. After that,
    // only the root-owned frozen copy is used for apply/commit/rollback.
    pub helper: PathBuf,
}

impl RecoveryRecord {
    pub(crate) fn new(candidate: &Path) -> Result<Self> {
        let candidate = candidate
            .canonicalize()
            .wrap_err("could not locate Linux Hand candidate")?;
        let metadata = fs::symlink_metadata(&candidate)?;
        if !metadata.is_file() {
            bail!("Linux Hand candidate must be a regular executable");
        }
        Ok(Self {
            transaction: uuid::Uuid::new_v4().to_string(),
            candidate_sha256: hex::encode(Sha256::digest(fs::read(&candidate)?)),
            helper: candidate,
        })
    }
    fn validate(&self) -> Result<()> {
        let id =
            uuid::Uuid::parse_str(&self.transaction).wrap_err("invalid Linux Hand transaction")?;
        if id.is_nil()
            || id.to_string() != self.transaction
            || self.candidate_sha256.len() != 64
            || !self
                .candidate_sha256
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            || !self.helper.is_absolute()
            || !crate::hand_executable::is_hand_file_name(&self.helper)
        {
            bail!("invalid Linux Hand recovery record");
        }
        Ok(())
    }
}

pub(crate) struct ServiceUpdate {
    record: RecoveryRecord,
}

impl ServiceUpdate {
    pub(crate) async fn prepare(record: RecoveryRecord) -> Result<Self> {
        record.validate()?;
        authorize().await?;
        let response = invoke(&record, "prepare").await?;
        let frozen = response["candidate_release"]
            .as_str()
            .or_else(|| response["release"].as_str())
            .ok_or_else(|| eyre!("Linux update receipt lacks frozen release"))?;
        let release = Path::new(frozen);
        if release.parent() != Some(Path::new(ROOT).join("releases").as_path()) {
            bail!("Linux update receipt points outside the installation");
        }
        let record = RecoveryRecord {
            helper: release.join("nanocodex2"),
            ..record
        };
        verify_frozen(&record)?;
        Ok(Self { record })
    }
    pub(crate) fn recovery_record(&self) -> &RecoveryRecord {
        &self.record
    }
    pub(crate) async fn apply(&mut self) -> Result<()> {
        let response = invoke(&self.record, "apply").await?;
        require_phase(&response, "applied")?;
        if response["runtime_verified"] != true
            || response["current_sha256"] != self.record.candidate_sha256
            || response["pid"].as_u64().is_none_or(|pid| pid == 0)
        {
            bail!(
                "Selected Linux Hand is not verified as the independently running service; rollback required"
            );
        }
        Ok(())
    }
    pub(crate) async fn rollback(&mut self) -> Result<()> {
        // Reconcile a possibly interrupted restart under the same operation ID,
        // never retry an uncertain activation under another transaction.
        invoke(&self.record, "recover").await?;
        let response = invoke(&self.record, "rollback").await?;
        require_phase(&response, "rolledBack")
    }
    pub(crate) async fn commit(&mut self) -> Result<()> {
        let response = invoke(&self.record, "commit").await?;
        require_phase(&response, "committed")
    }
}

fn verify_frozen(record: &RecoveryRecord) -> Result<()> {
    use std::os::unix::fs::MetadataExt as _;
    let meta = fs::symlink_metadata(&record.helper)?;
    if !meta.is_file()
        || meta.uid() != 0
        || meta.mode() & 0o022 != 0
        || meta.nlink() != 1
        || hex::encode(Sha256::digest(fs::read(&record.helper)?)) != record.candidate_sha256
    {
        bail!("Linux Hand frozen candidate failed verification");
    }
    Ok(())
}

pub(crate) async fn recover(record: &RecoveryRecord, committed: bool) -> Result<()> {
    record.validate()?;
    authorize().await?;
    let response = invoke(record, "recover").await?;
    // Preparation may have stopped before a frozen executable was created. The
    // root helper has already guarded and abandoned that same operation without
    // touching the installed Hand. Do not require or execute a missing copy.
    if !committed && response["phase"] == "rolledBack" {
        return Ok(());
    }
    let frozen = response["candidate_release"]
        .as_str()
        .or_else(|| response["release"].as_str())
        .ok_or_else(|| eyre!("Linux recovery receipt lacks frozen release"))?;
    let release = Path::new(frozen);
    if release.parent() != Some(Path::new(ROOT).join("releases").as_path()) {
        bail!("Linux recovery receipt points outside installation");
    }
    let mut update = ServiceUpdate {
        record: RecoveryRecord {
            helper: release.join("nanocodex2"),
            ..record.clone()
        },
    };
    verify_frozen(&update.record)?;
    if committed {
        update.commit().await
    } else {
        update.rollback().await
    }
}

fn require_phase(response: &Value, expected: &str) -> Result<()> {
    if response["phase"] != expected {
        bail!("Linux Hand transaction did not reach {expected}; recovery retained");
    }
    Ok(())
}

async fn invoke(record: &RecoveryRecord, action: &str) -> Result<Value> {
    record.validate()?;
    // sudo is an explicit device-administrator operation, not authority derived
    // from an account token. Background staging never reaches this boundary.
    let mut command = if nix::unistd::geteuid().is_root() {
        Command::new(&record.helper)
    } else {
        let mut command = Command::new("/usr/bin/sudo");
        command.args(["-n", "--"]).arg(&record.helper);
        command
    };
    command
        .arg("__update-hand")
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .wrap_err("could not invoke Linux Hand update helper")?;
    let request = json!({"protocol":1,"action":action,"transaction":record.transaction,
        "candidate_sha256":record.candidate_sha256,"start_stopped":true});
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| eyre!("Linux update stdin unavailable"))?;
    stdin.write_all(&serde_json::to_vec(&request)?).await?;
    drop(stdin);
    let output = tokio::time::timeout(Duration::from_secs(300), child.wait_with_output()).await
        .wrap_err("Linux Hand update outcome is uncertain; run nanocodex hand recover before another update")??;
    if !output.status.success() {
        bail!("Linux Hand {action} failed; transaction retained for nanocodex hand recover");
    }
    if output.stdout.len() > 65536 {
        bail!("Linux Hand receipt exceeded size limit");
    }
    let response: Value =
        serde_json::from_slice(&output.stdout).wrap_err("invalid Linux Hand update receipt")?;
    if response["protocol"] != 1
        || response["transaction"] != record.transaction
        || response["candidate_sha256"] != record.candidate_sha256
    {
        bail!("Linux Hand receipt does not match selected transaction and binary");
    }
    Ok(response)
}

pub(crate) async fn authorize() -> Result<()> {
    if nix::unistd::geteuid().is_root() {
        return Ok(());
    }
    let cached = Command::new("/usr/bin/sudo")
        .args(["-n", "true"])
        .status()
        .await?;
    if cached.success() {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        bail!(
            "Device administrator authorization is required to update the system Hand; background updates only stage the verified bundle"
        );
    }
    if !Command::new("/usr/bin/sudo")
        .arg("-v")
        .status()
        .await?
        .success()
    {
        bail!("Device administrator authorization was not granted");
    }
    Ok(())
}

pub(crate) async fn service_action(action: &str) -> Result<()> {
    if !matches!(action, "start" | "stop" | "restart") {
        bail!("unsupported Hand service action");
    }
    authorize().await?;
    let mut command = if nix::unistd::geteuid().is_root() {
        Command::new(SYSTEMCTL)
    } else {
        let mut c = Command::new("/usr/bin/sudo");
        c.args(["-n", "--", SYSTEMCTL]);
        c
    };
    if !command.args([action, UNIT]).status().await?.success() {
        bail!("could not {action} the Linux Hand service");
    }
    Ok(())
}

pub(crate) async fn validate_candidate(candidate: &Path) -> Result<()> {
    let mut child = Command::new(candidate)
        .arg("__update-hand")
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| eyre!("candidate protocol input unavailable"))?;
    input
        .write_all(b"{\"protocol\":1,\"action\":\"capabilities\"}")
        .await?;
    drop(input);
    let output = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output()).await??;
    let value: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
    if !output.status.success() || value["protocol"] != 1 || value["systemdHandUpdate"] != true {
        bail!(
            "Selected Linux Hand does not support transactional systemd updates; select a release/branch/PR with the Linux Hand updater"
        );
    }
    Ok(())
}
