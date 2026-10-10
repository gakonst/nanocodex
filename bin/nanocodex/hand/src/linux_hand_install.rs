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
const COMPONENTS_SERVICE: &str = "nanocodex-hand-components.service";
// Native headless screens are video-only. Provision their encoder alongside
// display/input prerequisites; users must not repair a fresh install over SSH.
const DEBIAN_DESKTOP_PACKAGES: &[&str] = &[
    "ca-certificates",
    "libpulse0",
    "libxkbcommon0",
    "xvfb",
    "openbox",
    "xterm",
    "xauth",
    "fonts-dejavu-core",
    "ffmpeg",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    #[serde(default)]
    prepare: bool,
    #[serde(default)]
    prepare_components: bool,
    #[serde(default)]
    origin: String,
    #[serde(default)]
    credential: String,
    #[serde(default)]
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

    let mut bytes = Vec::new();
    std::io::stdin()
        .take(64 * 1024)
        .read_to_end(&mut bytes)
        .context("could not read the Hand installation request")?;
    let request: Request =
        serde_json::from_slice(&bytes).context("invalid installation request")?;
    validate_request(&request)?;
    // The OS owns this job; dependency downloads survive installer/login exit.
    // Its lock is independent of the enrollment transaction that starts it.
    if request.prepare_components {
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open("/run/lock/nanocodex-hand-components.lock")?;
        lock.try_lock_exclusive()
            .context("Hand component preparation is already running")?;
        install_dependencies().await?;
        println!(
            "{}",
            json!({"status":"ready", "components":"native_desktop"})
        );
        return Ok(());
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

    let root = Path::new(ROOT);
    let state = Path::new(STATE);
    safe_directory(root)?;
    safe_directory(state)?;
    let record_path = root.join("installation.json");
    let previous = read_record(&record_path)?;
    if request.prepare
        && let Some(previous) = &previous
    {
        if Path::new("/etc/systemd/system")
            .join(COMPONENTS_SERVICE)
            .is_file()
        {
            checked(
                Command::new("systemctl").args(["start", "--no-block", COMPONENTS_SERVICE]),
                "resume background desktop preparation",
            )
            .await?;
        }
        println!(
            "{}",
            json!({"status": "installed", "awaiting_login": previous["pending_login"] == true})
        );
        return Ok(());
    }
    if let Some(previous) = &previous {
        if previous["pending_login"] != true {
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
        }
        if previous.get("native_only") == Some(&Value::Bool(false))
            || previous.get("mode").and_then(Value::as_str) == Some("factory")
        {
            bail!(
                "the existing installation owns retained VM factory state; migrate it explicitly before installing a native-only Hand"
            );
        }
    }

    let service_user = service_user(previous.as_ref()).await?;
    prepare_state(&service_user).await?;
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
    let secret_changed = if request.prepare {
        false
    } else {
        atomic_write(&root.join("account.env"), account.as_bytes(), 0o600)?
    };
    let link_changed = activate(root, &release)?;
    let unit_changed = atomic_write(
        Path::new("/etc/systemd/system").join(SERVICE).as_path(),
        service_unit(&service_user).as_bytes(),
        0o644,
    )?;
    atomic_write(
        Path::new("/etc/systemd/system")
            .join(COMPONENTS_SERVICE)
            .as_path(),
        components_unit().as_bytes(),
        0o644,
    )?;
    let public = json!({
        "pending_login": request.prepare,
        "service_uid": service_user.uid.as_raw(),
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
        Command::new("systemctl").args(["start", "--no-block", COMPONENTS_SERVICE]),
        "start background desktop preparation",
    )
    .await?;
    if request.prepare {
        eprintln!(
            "Desktop dependencies are preparing in the background; account sign-in can continue."
        );
        println!(
            "{}",
            json!({"status": "prepared", "awaiting_login": true, "revision": revision})
        );
        return Ok(());
    }
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

    let machine = describe_machine(&request, &service_user).await?;
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
    if request.prepare || request.prepare_components {
        if request.prepare && request.prepare_components {
            bail!("select one preparation phase");
        }
        if !request.origin.is_empty() || !request.credential.is_empty() || !request.owner.is_empty()
        {
            bail!("preparation must not contain account credentials");
        }
        return Ok(());
    }
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

// Omarchy uses Arch's package database, not Debian package names. Do not
// infer support merely from an unrelated package-manager binary on PATH.
const ARCH_DESKTOP_PACKAGES: &[&str] = &[
    "ca-certificates",
    "libpulse",
    "libxkbcommon",
    "xorg-server-xvfb",
    "openbox",
    "xterm",
    "xorg-xauth",
    "ttf-dejavu",
    "ffmpeg",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PackageManager {
    Apt,
    Pacman,
}

impl PackageManager {
    fn detect(os_release: &str) -> Result<Self> {
        // os-release is data, never a script to source. ID_LIKE alone does not
        // opt an untested derivative into automatic root-side installation.
        let id = os_release.lines().find_map(|line| {
            line.trim()
                .strip_prefix("ID=")
                .map(|value| value.trim().trim_matches(['\"', '\'']))
        });
        match id {
            Some("debian" | "ubuntu") => Ok(Self::Apt),
            Some("arch" | "omarchy") => Ok(Self::Pacman),
            _ => bail!(
                "automatic dependency installation supports Debian, Ubuntu, Arch Linux and Omarchy only"
            ),
        }
    }

    fn packages(self) -> &'static [&'static str] {
        match self {
            Self::Apt => DEBIAN_DESKTOP_PACKAGES,
            Self::Pacman => ARCH_DESKTOP_PACKAGES,
        }
    }

    fn probes(self) -> Vec<DependencyCommand> {
        match self {
            Self::Apt => vec![
                DependencyCommand::new("apt-get", &["--version"]),
                DependencyCommand::new("dpkg-query", &["--version"]),
            ],
            Self::Pacman => vec![DependencyCommand::new("pacman", &["--version"])],
        }
    }

    fn query(self, package: &'static str) -> DependencyCommand {
        match self {
            Self::Apt => DependencyCommand::new("dpkg-query", &["-W", "-f=${Status}", package]),
            Self::Pacman => DependencyCommand::new("pacman", &["-Q", package]),
        }
    }

    fn installed(self, success: bool, stdout: &[u8]) -> bool {
        success && (self == Self::Pacman || stdout == b"install ok installed")
    }

    fn install_plan(self, missing: &[&'static str]) -> Vec<DependencyCommand> {
        if missing.is_empty() {
            return Vec::new();
        }
        match self {
            Self::Apt => {
                let mut update = DependencyCommand::new("apt-get", &["update"]);
                update.env.push(("DEBIAN_FRONTEND", "noninteractive"));
                let mut install = DependencyCommand::new(
                    "apt-get",
                    &["install", "-y", "--no-install-recommends"],
                );
                install.args.extend_from_slice(missing);
                install.env.extend([
                    ("DEBIAN_FRONTEND", "noninteractive"),
                    ("NEEDRESTART_MODE", "l"),
                ]);
                vec![update, install]
            }
            Self::Pacman => {
                // Install from the EXISTING sync database only. pacman(8)
                // defines -y as refresh and -u as system upgrade; neither is
                // authorized by Hand setup. Never retry with -Sy or -Syu.
                let mut install =
                    DependencyCommand::new("pacman", &["-S", "--noconfirm", "--needed"]);
                install.args.extend_from_slice(missing);
                vec![install]
            }
        }
    }

    fn install_failure_context(self, missing: &[&str]) -> String {
        match self {
            Self::Apt => "could not install desktop dependencies".into(),
            Self::Pacman => format!(
                "Arch/Omarchy dependency installation failed against the existing sync database; no database refresh or host-wide upgrade was attempted. If the database is stale or dependencies conflict, have an administrator explicitly approve and run `pacman -Syu --needed {}`, then rerun Hand setup. The installer will not retry or refresh automatically",
                missing.join(" ")
            ),
        }
    }
}

// Pure command plans permit fixture tests without package installs, sudo,
// service changes or writes to system paths. Execution stays root-side only.
#[derive(Debug, PartialEq, Eq)]
struct DependencyCommand {
    program: &'static str,
    args: Vec<&'static str>,
    env: Vec<(&'static str, &'static str)>,
}

impl DependencyCommand {
    fn new(program: &'static str, args: &[&'static str]) -> Self {
        Self {
            program,
            args: args.to_vec(),
            env: Vec::new(),
        }
    }

    fn command(&self) -> Command {
        self.command_at(Path::new(self.program))
    }

    fn command_at(&self, program: &Path) -> Command {
        let mut command = Command::new(program);
        command
            .args(&self.args)
            .envs(self.env.iter().copied())
            .stdin(Stdio::null())
            .kill_on_drop(true);
        command
    }
}

async fn install_dependencies() -> Result<()> {
    let os_release =
        fs::read_to_string("/etc/os-release").context("could not detect the Linux distribution")?;
    let manager = PackageManager::detect(&os_release)?;
    for probe in manager.probes() {
        checked(
            probe.command().stdout(Stdio::null()).stderr(Stdio::null()),
            "verify the distribution package manager",
        )
        .await?;
    }
    let mut missing = Vec::new();
    for &package in manager.packages() {
        let output = manager
            .query(package)
            .command()
            .output()
            .await
            .context("could not query desktop dependencies")?;
        if !manager.installed(output.status.success(), &output.stdout) {
            missing.push(package);
        }
    }
    if !missing.is_empty() {
        let plan = manager.install_plan(&missing);
        eprintln!("Installing Linux desktop dependencies…");
        if manager == PackageManager::Pacman {
            eprintln!(
                "Using the existing Arch/Omarchy sync database without refreshing it or upgrading the host."
            );
        }
        for step in plan {
            checked(&mut step.command(), "install desktop dependencies")
                .await
                .with_context(|| manager.install_failure_context(&missing))?;
        }
    }
    // Even an already-installed ffmpeg can be a custom build without x264.
    // Validate before creating users, credentials or starting the Hand service.
    verify_ffmpeg().await
}

fn ffmpeg_has_capability(table: &[u8], name: &str, flag: char) -> bool {
    String::from_utf8_lossy(table).lines().any(|line| {
        let mut fields = line.split_whitespace();
        match (fields.next(), fields.next()) {
            (Some(flags), Some(entry)) => flags.contains(flag) && entry == name,
            _ => false,
        }
    })
}

fn ffmpeg_smoke_plan() -> DependencyCommand {
    DependencyCommand::new(
        "ffmpeg",
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "color=size=64x64:rate=1",
            "-frames:v",
            "1",
            "-an",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-tune",
            "zerolatency",
            "-pix_fmt",
            "yuv420p",
            "-f",
            "h264",
            "pipe:1",
        ],
    )
}

async fn verify_ffmpeg() -> Result<()> {
    verify_ffmpeg_at(Path::new("ffmpeg")).await
}

async fn verify_ffmpeg_at(program: &Path) -> Result<()> {
    for (table, name, flag) in [("-encoders", "libx264", 'V'), ("-devices", "x11grab", 'D')] {
        let mut command =
            DependencyCommand::new("ffmpeg", &["-hide_banner", table]).command_at(program);
        let output = tokio::time::timeout(Duration::from_secs(15), command.output())
            .await
            .context("ffmpeg capability query timed out")?
            .context("could not verify ffmpeg capabilities")?;
        if !output.status.success() || !ffmpeg_has_capability(&output.stdout, name, flag) {
            bail!(
                "ffmpeg lacks required {name} capability; install a full distribution ffmpeg build"
            );
        }
    }
    let mut command = ffmpeg_smoke_plan().command_at(program);
    command.stdout(Stdio::null()).stderr(Stdio::null());
    tokio::time::timeout(
        Duration::from_secs(15),
        checked(&mut command, "verify ffmpeg H.264 encoding"),
    )
    .await
    .context("ffmpeg H.264 encoding verification timed out")?
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

// Retained installations keep their OS owner. A new sudo install belongs to
// the invoking login, so its CLI can share private IPC without widening access.
async fn service_user(previous: Option<&Value>) -> Result<nix::unistd::User> {
    use std::os::unix::fs::MetadataExt;
    let uid = if previous.is_some() {
        nix::unistd::Uid::from_raw(fs::metadata(STATE)?.uid())
    } else if let Some(uid) = std::env::var("SUDO_UID")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|uid| *uid != 0)
    {
        nix::unistd::Uid::from_raw(uid)
    } else {
        ensure_user().await?;
        return nix::unistd::User::from_name("nanocodex")?.context("missing service user");
    };
    if uid.is_root() {
        bail!("retained Hand state has no non-root service owner; repair ownership explicitly");
    }
    nix::unistd::User::from_uid(uid)?.context("Hand service owner no longer exists")
}

async fn prepare_state(user: &nix::unistd::User) -> Result<()> {
    for path in [
        PathBuf::from(STATE),
        Path::new(STATE).join("workspace"),
        Path::new(STATE).join("desktop"),
        Path::new(STATE).join("cache"),
    ] {
        safe_directory(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        checked(
            Command::new("chown").args([
                &format!("{}:{}", user.uid, user.gid),
                path.to_string_lossy().as_ref(),
            ]),
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

fn service_unit(user: &nix::unistd::User) -> String {
    r#"[Unit]
Description=Nanocodex host Hand
Wants=network-online.target
Requires=nanocodex-hand-components.service
After=network-online.target nanocodex-hand-components.service
ConditionPathExists=/opt/nanocodex/account.env
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
    .replace("User=nanocodex", &format!("User={}", user.uid))
    .replace("Group=nanocodex", &format!("Group={}", user.gid))
}

fn components_unit() -> &'static str {
    r#"[Unit]
Description=Nanocodex desktop component preparation
Wants=network-online.target
After=network-online.target

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/opt/nanocodex/current/nanocodex2 __install-hand
StandardInput=data
StandardInputText={"prepare_components":true}
TimeoutStartSec=15min
UMask=0077
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

async fn describe_machine(request: &Request, user: &nix::unistd::User) -> Result<String> {
    let output = Command::new("runuser")
        .args([
            "-u",
            &user.name,
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
    nanocodex::oai::transport::install_default_rustls_crypto_provider();
    eprintln!("Checking the account Hand and screen catalog…");
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .build()?;
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        let hands = account_get(&client, request, "/v1/account/hands").await;
        let screens = account_get(&client, request, "/v1/account/hands/screens").await;
        if let (Ok(hands), Ok(screens)) = (hands, screens)
            && catalog_ready(&hands, &screens, machine)
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    bail!(
        "the service started but its Hand and controllable video desktop did not appear in the account catalog"
    )
}

fn catalog_ready(hands: &Value, screens: &Value, machine: &str) -> bool {
    // /v1/account/hands already filters offline machines server-side. Do not
    // invent an `online` field or accept a screen without its attached Hand.
    let hand_ready = hands["data"]
        .as_array()
        .is_some_and(|hands| hands.iter().any(|hand| hand["id"] == machine));
    let screen_ready = screens["surfaces"].as_array().is_some_and(|screens| {
        screens.iter().any(|screen| {
            screen["machine_id"] == machine
                && screen["id"] == "desktop"
                && screen["kind"] == "desktop"
                && screen["controllable"] == true
                && ["width", "height"].iter().all(|key| {
                    screen[key].as_u64().is_some_and(|size| (1..=16384).contains(&size))
                })
                // WebRTC is the existing catalog default (no transport field),
                // not a literal `webrtc` value. Frame catalogs are not ready.
                && screen.get("transport").is_none()
                && screen.get("frame_window").is_none()
        })
    });
    hand_ready && screen_ready
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
            prepare: false,
            prepare_components: false,
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
                prepare: false,
                prepare_components: false,
                origin: origin.into(),
                credential: valid.credential.clone(),
                owner: valid.owner.clone(),
            };
            assert!(validate_request(&invalid).is_err(), "{origin}");
        }
    }

    fn ready_catalogs() -> (Value, Value) {
        (
            json!({"data": [{"id": "fixture"}]}),
            json!({"surfaces": [{"machine_id": "fixture", "id": "desktop",
                "kind": "desktop", "controllable": true, "width": 1920, "height": 1080}]}),
        )
    }

    #[test]
    fn headless_dependencies_include_the_required_video_encoder() {
        for package in [
            "xvfb",
            "openbox",
            "xterm",
            "xauth",
            "fonts-dejavu-core",
            "ffmpeg",
        ] {
            assert!(
                DEBIAN_DESKTOP_PACKAGES.contains(&package),
                "missing {package}"
            );
        }
    }

    #[test]
    fn distro_detection_is_explicit_and_does_not_source_os_release() {
        for fixture in [
            "ID=debian\n",
            "NAME=Ubuntu\nID=\"ubuntu\"\n",
            "ID='ubuntu'\n",
        ] {
            assert_eq!(
                PackageManager::detect(fixture).unwrap(),
                PackageManager::Apt
            );
        }
        for fixture in [
            "ID=arch\n",
            "ID=\"arch\"\nNAME=Omarchy\n",
            "ID=omarchy\nID_LIKE=arch\n",
        ] {
            assert_eq!(
                PackageManager::detect(fixture).unwrap(),
                PackageManager::Pacman
            );
        }
        for fixture in [
            "",
            "ID=fedora\n",
            "ID=manjaro\nID_LIKE=arch\n",
            "ID_LIKE=debian\n",
            "ID=$(echo arch)\n",
        ] {
            assert!(PackageManager::detect(fixture).is_err(), "{fixture}");
        }
    }

    #[test]
    fn arch_has_distribution_specific_headless_dependencies() {
        for package in [
            "ca-certificates",
            "libpulse",
            "libxkbcommon",
            "xorg-server-xvfb",
            "openbox",
            "xterm",
            "xorg-xauth",
            "ttf-dejavu",
            "ffmpeg",
        ] {
            assert!(
                PackageManager::Pacman.packages().contains(&package),
                "{package}"
            );
        }
        for debian_only in [
            "libpulse0",
            "libxkbcommon0",
            "xvfb",
            "xauth",
            "fonts-dejavu-core",
        ] {
            assert!(!PackageManager::Pacman.packages().contains(&debian_only));
        }
    }

    #[test]
    fn fixture_package_queries_distinguish_installed_and_missing() {
        let apt = PackageManager::Apt;
        assert_eq!(
            apt.query("ffmpeg"),
            DependencyCommand::new("dpkg-query", &["-W", "-f=${Status}", "ffmpeg"])
        );
        assert!(apt.installed(true, b"install ok installed"));
        assert!(!apt.installed(true, b"deinstall ok config-files"));
        assert!(!apt.installed(false, b"install ok installed"));
        let arch = PackageManager::Pacman;
        assert_eq!(
            arch.query("ffmpeg"),
            DependencyCommand::new("pacman", &["-Q", "ffmpeg"])
        );
        assert!(arch.installed(true, b"ffmpeg 8.0-1\n"));
        assert!(!arch.installed(false, b""));
        assert_eq!(
            apt.probes()
                .iter()
                .map(|step| step.program)
                .collect::<Vec<_>>(),
            ["apt-get", "dpkg-query"]
        );
        assert_eq!(
            arch.probes(),
            [DependencyCommand::new("pacman", &["--version"])]
        );
    }

    #[test]
    fn arch_install_plan_uses_existing_database_without_refresh_or_system_upgrade() {
        let steps = PackageManager::Pacman.install_plan(&["ffmpeg", "xorg-server-xvfb"]);
        assert_eq!(
            steps,
            [DependencyCommand::new(
                "pacman",
                &[
                    "-S",
                    "--noconfirm",
                    "--needed",
                    "ffmpeg",
                    "xorg-server-xvfb"
                ]
            )]
        );
        for forbidden in [
            "-Sy",
            "-Syu",
            "-Su",
            "--refresh",
            "--sysupgrade",
            "--ignore",
            "--nodeps",
        ] {
            assert!(!steps[0].args.contains(&forbidden));
        }
        assert!(PackageManager::Pacman.install_plan(&[]).is_empty());
        assert!(PackageManager::Apt.install_plan(&[]).is_empty());
    }

    #[test]
    fn arch_install_failure_requires_administrator_upgrade_approval_without_retry() {
        let diagnostic =
            PackageManager::Pacman.install_failure_context(&["ffmpeg", "xorg-server-xvfb"]);
        assert!(diagnostic.contains("existing sync database"));
        assert!(diagnostic.contains("no database refresh or host-wide upgrade was attempted"));
        assert!(diagnostic.contains("explicitly approve"));
        assert!(diagnostic.contains("pacman -Syu --needed ffmpeg xorg-server-xvfb"));
        assert!(diagnostic.contains("rerun Hand setup"));
        assert!(diagnostic.contains("will not retry or refresh automatically"));
    }

    #[test]
    fn debian_plan_installs_only_missing_packages_noninteractively_without_sudo() {
        let steps = PackageManager::Apt.install_plan(&["ffmpeg"]);
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].program, "apt-get");
        assert_eq!(steps[0].args, ["update"]);
        assert_eq!(
            steps[1].args,
            ["install", "-y", "--no-install-recommends", "ffmpeg"]
        );
        for step in &steps {
            assert!(step.env.contains(&("DEBIAN_FRONTEND", "noninteractive")));
            assert_ne!(step.program, "sudo");
        }
        assert!(steps[1].env.contains(&("NEEDRESTART_MODE", "l")));
    }

    #[test]
    fn ffmpeg_fixtures_require_exact_video_encoder_and_capture_device() {
        assert!(ffmpeg_has_capability(
            b"Encoders:\n V....D libx264 libx264 H.264 / AVC\n",
            "libx264",
            'V'
        ));
        for fixture in [
            " A..... libx264 wrong media type\n",
            " V..... libx264rgb not the encoder used\n",
            " V..... h264 description mentions libx264\n",
            "",
            " V..... = Video\n",
        ] {
            assert!(!ffmpeg_has_capability(fixture.as_bytes(), "libx264", 'V'));
        }
        assert!(ffmpeg_has_capability(
            b" D  x11grab X11 screen capture\n",
            "x11grab",
            'D'
        ));
        assert!(!ffmpeg_has_capability(
            b" E  x11grab output only\n",
            "x11grab",
            'D'
        ));
        let smoke = ffmpeg_smoke_plan();
        assert!(
            smoke
                .args
                .windows(2)
                .any(|pair| pair == ["-c:v", "libx264"])
        );
        assert_eq!(smoke.args.last(), Some(&"pipe:1"));
        assert!(smoke.args.contains(&"-nostdin"));
    }

    #[tokio::test]
    async fn ffmpeg_fixture_command_checks_queries_and_actual_encoding() {
        // Only private temporary fixtures execute. No OS packages or service
        // changes and no PATH mutation, allowing concurrent tests safely.
        let dir = tempfile::tempdir().unwrap();
        let fixture = dir.path().join("ffmpeg-fixture");
        for (encoders, devices, encode_status, expected) in [
            ("V....D libx264 H264", "D x11grab capture", 0, true),
            ("V....D libx264rgb H264", "D x11grab capture", 0, false),
            ("V....D libx264 H264", "E x11grab output", 0, false),
            ("V....D libx264 H264", "D x11grab capture", 1, false),
        ] {
            fs::write(&fixture, format!("#!/bin/sh\ncase \"$2\" in\n-encoders) printf '%s\\n' '{encoders}';;\n-devices) printf '%s\\n' '{devices}';;\n*) exit {encode_status};;\nesac\n")).unwrap();
            fs::set_permissions(&fixture, fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(verify_ffmpeg_at(&fixture).await.is_ok(), expected);
        }
    }

    #[test]
    fn readiness_accepts_the_actual_default_webrtc_catalog_without_online_field() {
        let (hands, screens) = ready_catalogs();
        assert!(hands["data"][0].get("online").is_none());
        assert!(screens["surfaces"][0].get("transport").is_none());
        assert!(catalog_ready(&hands, &screens, "fixture"));
    }

    #[test]
    fn readiness_rejects_frame_catalogs_and_foreign_or_unusable_surfaces() {
        let (hands, valid) = ready_catalogs();
        for (key, value) in [
            ("transport", json!("frames-v1")),
            ("transport", json!("webrtc")),
            ("transport", Value::Null),
            ("frame_window", json!(1)),
            ("machine_id", json!("other")),
            ("id", json!("phone")),
            ("kind", json!("window")),
            ("controllable", json!(false)),
            ("width", json!(0)),
            ("height", json!(16385)),
            ("width", json!("1920")),
        ] {
            let mut screens = valid.clone();
            screens["surfaces"][0][key] = value;
            assert!(
                !catalog_ready(&hands, &screens, "fixture"),
                "accepted {key}"
            );
        }
    }

    #[test]
    fn readiness_requires_both_the_connected_hand_and_its_video_desktop() {
        let (hands, screens) = ready_catalogs();
        assert!(!catalog_ready(&json!({"data": []}), &screens, "fixture"));
        assert!(!catalog_ready(
            &json!({"data": [{"id": "other"}]}),
            &screens,
            "fixture"
        ));
        assert!(!catalog_ready(&hands, &json!({"surfaces": []}), "fixture"));
        assert!(!catalog_ready(&Value::Null, &screens, "fixture"));
        assert!(!catalog_ready(&hands, &Value::Null, "fixture"));
    }

    #[test]
    fn service_runs_only_the_activated_native_binary() {
        let user = nix::unistd::User::from_uid(nix::unistd::geteuid())
            .unwrap()
            .unwrap();
        let unit = service_unit(&user);
        assert!(unit.contains("ExecStart=/opt/nanocodex/current/nanocodex2 hand"));
        assert!(!unit.contains("python"));
        assert!(!unit.contains("bash"));
        assert!(unit.contains(&format!("User={}\nGroup={}", user.uid, user.gid)));
        assert!(unit.contains("UMask=0077"));
        assert!(!unit.contains("sudo"));
        assert!(!unit.contains("NANOCODEX_DESKTOP_TARGET"));
        assert!(!unit.contains("ncx_live_"));
    }
}
