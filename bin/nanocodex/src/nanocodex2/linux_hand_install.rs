//! Root-side Linux Hand installation. The controller sends one JSON request on
//! stdin; no credential is accepted through argv, files in /tmp, or logs.

use anyhow::{Context, Result, bail};
use fs2::FileExt as _;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{ErrorKind, Read, Write},
    os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _, symlink},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::process::Command;

const ROOT: &str = "/opt/nanocodex";
const STATE: &str = "/srv/nanocodex";
const SERVICE: &str = "nanocodex-hand.service";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    origin: String,
    credential: String,
    owner: String,
}

pub(super) async fn run() -> Result<(), nanocodex_managed::ManagedError> {
    install()
        .await
        .map_err(|error| nanocodex_managed::ManagedError::Configuration(format!("{error:#}")))
}

async fn install() -> Result<()> {
    if !nix::unistd::geteuid().is_root() {
        bail!("the native Linux Hand installer must run as root");
    }
    if std::env::consts::ARCH != "x86_64" {
        bail!("automatic Linux Hand installation currently requires x86_64");
    }
    if !Path::new("/run/systemd/system").is_dir() {
        bail!("systemd must be running");
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open("/run/lock/nanocodex-hand-setup.lock")?;
    lock.try_lock_exclusive()
        .context("another Hand installation is already running")?;

    let mut bytes = Vec::new();
    std::io::stdin()
        .take(64 * 1024)
        .read_to_end(&mut bytes)
        .context("could not read the Hand installation request")?;
    let request: Request =
        serde_json::from_slice(&bytes).context("invalid installation request")?;
    validate_request(&request)?;

    let root = Path::new(ROOT);
    let state = Path::new(STATE);
    safe_directory(root)?;
    safe_directory(state)?;
    let record_path = root.join("installation.json");
    let previous = read_record(&record_path)?;
    if let Some(previous) = &previous {
        for (field, expected) in [
            ("owner", request.owner.as_str()),
            ("origin", request.origin.as_str()),
        ] {
            if previous.get(field).and_then(Value::as_str) != Some(expected) {
                bail!(
                    "existing Hand installation belongs to another {field}; refusing to replace retained state"
                );
            }
        }
        if previous.get("native_only") == Some(&Value::Bool(false))
            || previous.get("mode").and_then(Value::as_str) == Some("factory")
        {
            bail!(
                "the existing installation owns retained VM factory state; migrate it explicitly before installing a native-only Hand"
            );
        }
    }

    install_dependencies().await?;
    ensure_user().await?;
    prepare_state().await?;
    for child in ["cache", "releases"] {
        safe_directory(&root.join(child))?;
    }

    let executable = std::env::current_exe().context("could not locate the Hand installer")?;
    let binary = fs::read(&executable)
        .with_context(|| format!("could not read {}", executable.display()))?;
    validate_linux_binary(&binary)?;
    let revision = hex::encode(Sha256::digest(&binary))[..24].to_owned();
    let release = root.join("releases").join(&revision);
    safe_directory(&release)?;
    let installed = release.join("nanocodex2");
    let binary_changed = atomic_write(&installed, &binary, 0o755)?;
    checked(
        Command::new(&installed).arg("--version"),
        "verify nanocodex2",
    )
    .await?;

    let account = format!(
        "NANOCODEX_API_KEY={}\nNANOCODEX_MANAGED_URL={}\n",
        request.credential, request.origin
    );
    let secret_changed = atomic_write(&root.join("account.env"), account.as_bytes(), 0o600)?;
    let link_changed = activate(root, &release)?;
    let unit_changed = atomic_write(
        Path::new("/etc/systemd/system").join(SERVICE).as_path(),
        service_unit().as_bytes(),
        0o644,
    )?;
    let public = json!({
        "owner": request.owner,
        "origin": request.origin,
        "mode": "native",
        "native_only": true,
        "revision": revision,
    });
    atomic_write(
        &record_path,
        format!("{}\n", serde_json::to_string_pretty(&public)?).as_bytes(),
        0o644,
    )?;

    checked(
        Command::new("systemctl").arg("daemon-reload"),
        "reload systemd",
    )
    .await?;
    checked(
        Command::new("systemctl").args(["enable", SERVICE]),
        "enable the Hand service",
    )
    .await?;
    let action = if binary_changed || secret_changed || link_changed || unit_changed {
        "restart"
    } else {
        "start"
    };
    checked(
        Command::new("systemctl").args([action, SERVICE]),
        "start the Hand service",
    )
    .await?;

    let machine = describe_machine(&request).await?;
    wait_ready(&request, &machine).await?;
    println!(
        "{}",
        serde_json::to_string(&json!({
            "status": "ready",
            "machine_id": machine,
            "mode": "native",
            "revision": revision,
        }))?
    );
    Ok(())
}

fn validate_request(request: &Request) -> Result<()> {
    if !request.credential.starts_with("ncx_live_")
        || request.credential.len() > 4096
        || !request
            .credential
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        bail!("invalid enrollment credential");
    }
    let origin = url::Url::parse(&request.origin).context("invalid managed origin")?;
    if !matches!(origin.scheme(), "http" | "https")
        || origin.host_str().is_none()
        || origin.username() != ""
        || origin.password().is_some()
        || request
            .origin
            .bytes()
            .any(|byte| byte.is_ascii_whitespace())
    {
        bail!("invalid managed origin");
    }
    if request.owner.is_empty()
        || request.owner.len() > 256
        || !request
            .owner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        bail!("invalid account owner");
    }
    Ok(())
}

fn validate_linux_binary(binary: &[u8]) -> Result<()> {
    if binary.get(..6) != Some(b"\x7fELF\x02\x01") || binary.get(18..20) != Some(b"\x3e\x00") {
        bail!("installer is not an x86_64 Linux executable");
    }
    Ok(())
}

fn read_record(path: &Path) -> Result<Option<Value>> {
    regular_file(path)?;
    match fs::read(path) {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes).context("invalid existing installation record")?,
        )),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn regular_file(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => bail!("refusing unexpected file at {}", path.display()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn safe_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
        Ok(_) => bail!("refusing unexpected install directory {}", path.display()),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            fs::create_dir_all(path)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

async fn install_dependencies() -> Result<()> {
    if !Command::new("apt-get")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
    {
        bail!("automatic dependency installation currently supports Debian and Ubuntu");
    }
    let packages = [
        "ca-certificates",
        "libpulse0",
        "libxkbcommon0",
        "xvfb",
        "openbox",
        "xterm",
        "xauth",
        "fonts-dejavu-core",
    ];
    let mut missing = Vec::new();
    for package in packages {
        let output = Command::new("dpkg-query")
            .args(["-W", "-f=${Status}", package])
            .output()
            .await?;
        if !output.status.success() || output.stdout != b"install ok installed" {
            missing.push(package);
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    eprintln!("Installing Linux desktop dependencies…");
    let mut update = Command::new("apt-get");
    update
        .arg("update")
        .env("DEBIAN_FRONTEND", "noninteractive");
    checked(&mut update, "update package metadata").await?;
    let mut install = Command::new("apt-get");
    install
        .args(["install", "-y", "--no-install-recommends"])
        .args(missing)
        .env("DEBIAN_FRONTEND", "noninteractive")
        .env("NEEDRESTART_MODE", "l");
    checked(&mut install, "install desktop dependencies").await
}

async fn ensure_user() -> Result<()> {
    let exists = Command::new("id")
        .args(["-u", "nanocodex"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await?
        .success();
    if !exists {
        checked(
            Command::new("useradd").args([
                "--system",
                "--create-home",
                "--home-dir",
                STATE,
                "--shell",
                "/bin/sh",
                "nanocodex",
            ]),
            "create the nanocodex service user",
        )
        .await?;
    }
    Ok(())
}

async fn prepare_state() -> Result<()> {
    for path in [
        PathBuf::from(STATE),
        Path::new(STATE).join("workspace"),
        Path::new(STATE).join("desktop"),
        Path::new(STATE).join("cache"),
    ] {
        safe_directory(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        checked(
            Command::new("chown").args(["nanocodex:nanocodex", path.to_string_lossy().as_ref()]),
            "set Hand state ownership",
        )
        .await?;
    }
    Ok(())
}

fn atomic_write(path: &Path, contents: &[u8], mode: u32) -> Result<bool> {
    regular_file(path)?;
    if fs::read(path).is_ok_and(|existing| existing == contents) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
        return Ok(false);
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent", path.display()))?;
    safe_directory(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    temporary.write_all(contents)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("could not save {}", path.display()))?;
    Ok(true)
}

fn activate(root: &Path, release: &Path) -> Result<bool> {
    let current = root.join("current");
    if let Ok(metadata) = fs::symlink_metadata(&current)
        && !metadata.file_type().is_symlink()
    {
        bail!(
            "expected {} to be an installation symlink",
            current.display()
        );
    }
    if current.canonicalize().is_ok_and(|active| active == release) {
        return Ok(false);
    }
    let next = root.join(format!(".current-{}", std::process::id()));
    match fs::remove_file(&next) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    symlink(release, &next)?;
    if let Err(error) = fs::rename(&next, &current) {
        let _ = fs::remove_file(next);
        return Err(error.into());
    }
    Ok(true)
}

fn service_unit() -> &'static str {
    r#"[Unit]
Description=Nanocodex host Hand
Wants=network-online.target
After=network-online.target
StartLimitIntervalSec=0

[Service]
Type=simple
User=nanocodex
Group=nanocodex
WorkingDirectory=/srv/nanocodex/workspace
EnvironmentFile=/opt/nanocodex/account.env
Environment=HOME=/srv/nanocodex
Environment=NANOCODEX_DESKTOP_DATA=/srv/nanocodex/desktop
Environment=PATH=/usr/local/bin:/usr/bin:/bin
ExecStart=/opt/nanocodex/current/nanocodex2 hand
Restart=on-failure
RestartSec=5
TimeoutStopSec=90
KillMode=mixed
UMask=0077
CPUWeight=25

[Install]
WantedBy=multi-user.target
"#
}

async fn checked(command: &mut Command, operation: &str) -> Result<()> {
    let status = command
        .status()
        .await
        .with_context(|| format!("could not {operation}"))?;
    if !status.success() {
        bail!("could not {operation}: {status}");
    }
    Ok(())
}

async fn describe_machine(request: &Request) -> Result<String> {
    let output = Command::new("runuser")
        .args([
            "-u",
            "nanocodex",
            "--",
            "/opt/nanocodex/current/nanocodex2",
            "__device-hand",
            "--describe",
        ])
        .env("HOME", STATE)
        .env("NANOCODEX_API_KEY", &request.credential)
        .env("NANOCODEX_MANAGED_URL", &request.origin)
        .output()
        .await
        .context("could not resolve the Hand identity")?;
    if !output.status.success() {
        bail!(
            "could not resolve the Hand identity: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let identity: Value = serde_json::from_slice(&output.stdout)?;
    identity["id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("Hand returned an invalid identity"))
}

async fn wait_ready(request: &Request, machine: &str) -> Result<()> {
    eprintln!("Checking the account Hand and screen catalog…");
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .build()?;
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        let hands = account_get(&client, request, "/v1/account/hands").await;
        let screens = account_get(&client, request, "/v1/account/hands/screens").await;
        if let (Ok(hands), Ok(screens)) = (hands, screens) {
            let hand_ready = hands["data"]
                .as_array()
                .is_some_and(|hands| hands.iter().any(|hand| hand["id"] == machine));
            let screen_ready = screens["surfaces"].as_array().is_some_and(|screens| {
                screens.iter().any(|screen| screen["machine_id"] == machine)
            });
            if hand_ready && screen_ready {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    bail!("the service started but its Hand and desktop did not appear in the account catalog")
}

async fn account_get(client: &reqwest::Client, request: &Request, path: &str) -> Result<Value> {
    Ok(client
        .get(format!("{}{}", request.origin, path))
        .bearer_auth(&request.credential)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_validation_rejects_values_unsafe_for_environment_files() {
        let valid = Request {
            origin: "https://api.nanocodex.dev".into(),
            credential: "ncx_live_fixture-123".into(),
            owner: "user_fixture-123".into(),
        };
        assert!(validate_request(&valid).is_ok());
        for origin in [
            "file:///tmp/x",
            "https://ok.example\nEVIL=1",
            "https://u:p@x",
        ] {
            let invalid = Request {
                origin: origin.into(),
                credential: valid.credential.clone(),
                owner: valid.owner.clone(),
            };
            assert!(validate_request(&invalid).is_err(), "{origin}");
        }
    }

    #[test]
    fn service_runs_only_the_activated_native_binary() {
        let unit = service_unit();
        assert!(unit.contains("ExecStart=/opt/nanocodex/current/nanocodex2 hand"));
        assert!(!unit.contains("python"));
        assert!(!unit.contains("bash"));
    }
}
