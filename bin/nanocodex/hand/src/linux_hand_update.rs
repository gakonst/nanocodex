//! Narrow, journaled update of an existing Linux Hand. Never enrolls a Hand,
//! reads account.env, rewrites a unit, or restarts a factory/guest.
use anyhow::{Context, Result, bail};
use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write as _},
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _, symlink},
    path::{Component, Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

const ROOT: &str = "/opt/nanocodex";
const HAND: &str = "nanocodex-hand.service";
const EXE: &str = "/opt/nanocodex/current/nanocodex2";
const ANCILLARY: &str = "nanocodex-win11-webrtc.service";
const ANCILLARY_EXE: &str = "/usr/local/sbin/nanocodex-win11-webrtc";
const NOFOLLOW: i32 = nix::libc::O_NOFOLLOW;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    protocol: u32,
    action: Action,
    transaction: Option<String>,
    candidate_sha256: Option<String>,
    #[serde(default)]
    start_stopped: bool,
}
#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Action {
    Prepare,
    Apply,
    Commit,
    Rollback,
    Recover,
    Status,
    Capabilities,
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Entry {
    directory: bool,
    mode: u32,
    sha256: String,
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Pointer {
    target: PathBuf,
    dev: u64,
    ino: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessPin {
    pid: u32,
    start: String,
    uid: u32,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Factory {
    active_state: String,
    executable_sha256: String,
    executable: PathBuf,
    executable_dev: u64,
    executable_ino: u64,
    unit: String,
    cgroup: String,
    main: u32,
    pins: Vec<ProcessPin>,
    layout: BTreeMap<String, String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    protocol: u32,
    transaction: String,
    candidate_sha256: String,
    phase: String,
    candidate_complete: bool,
    was_active: bool,
    start_stopped: bool,
    original: Pointer,
    current: Pointer,
    pending: Option<Pointer>,
    pending_path: Option<PathBuf>,
    old_files: BTreeMap<String, Entry>,
    new_files: BTreeMap<String, Entry>,
    release: PathBuf,
    service_user: String,
    service_uid: u32,
    state_directory: PathBuf,
    machine_id: String,
    apply_started_millis: u64,
    layout: BTreeMap<String, String>,
    factories: Vec<Factory>,
    receipts: BTreeMap<String, String>,
}

pub(super) async fn run() -> Result<(), nanocodex_managed::ManagedError> {
    transaction().map_err(|error| {
        nanocodex_managed::ManagedError::Configuration(format!("Linux Hand update: {error:#}"))
    })
}

fn transaction() -> Result<()> {
    let mut input = Vec::new();
    std::io::stdin().take(16_385).read_to_end(&mut input)?;
    if input.len() > 16_384 {
        bail!("protocol request exceeds 16 KiB");
    }
    let request: Request =
        serde_json::from_slice(&input).context("invalid version-1 update request")?;
    if request.protocol != 1 {
        bail!("unsupported update protocol; expected protocol=1");
    }
    if request.action == Action::Capabilities {
        println!(
            "{}",
            serde_json::json!({"protocol":1,"systemdHandUpdate":cfg!(target_os = "linux")})
        );
        return Ok(());
    }
    if request.action == Action::Status {
        return status(request.transaction.as_deref());
    }
    if !nix::unistd::geteuid().is_root() {
        bail!("update actions require root; status is read-only");
    }
    if !cfg!(target_os = "linux") || !Path::new("/run/systemd/system").is_dir() {
        bail!("Linux with running systemd is required");
    }
    let id = valid_id(
        request
            .transaction
            .as_deref()
            .context("transaction UUID is required")?,
    )?;
    let sha = request
        .candidate_sha256
        .as_deref()
        .context("candidate_sha256 is required")?;
    if sha.len() != 64
        || !sha
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        bail!("expected lowercase 64-hex candidate_sha256");
    }
    // The caller cannot ask this privileged helper to run a different binary.
    let candidate = std::env::current_exe()?;
    let mut candidate_file = OpenOptions::new()
        .read(true)
        .custom_flags(NOFOLLOW)
        .open(&candidate)?;
    if !candidate_file.metadata()?.is_file() || digest(&mut candidate_file)? != sha {
        bail!("current executable does not match expected candidate SHA256");
    }
    candidate_file.rewind()?;
    validate_elf(&mut candidate_file)?;
    candidate_file.rewind()?;
    trusted_directory(Path::new("/opt"))?;
    trusted_directory(Path::new(ROOT))?;
    trusted_directory(&Path::new(ROOT).join("releases"))?;
    let lock = trusted_open(&Path::new(ROOT).join(".hand-update.lock"), true)?;
    lock.try_lock_exclusive()
        .context("another Hand update is in progress")?;
    // Cooperate with the installer too. /run/lock itself may be sticky, so never
    // follow a pre-created foreign lock and validate the resulting inode.
    let setup_lock = trusted_open(Path::new("/run/lock/nanocodex-hand-setup.lock"), true)?;
    setup_lock
        .try_lock_exclusive()
        .context("another Hand installation is in progress")?;
    let journals = Path::new(ROOT).join("hand-updates");
    if !journals.exists() {
        fs::create_dir(&journals)?;
        fs::set_permissions(&journals, fs::Permissions::from_mode(0o700))?;
        sync_dir(Path::new(ROOT))?;
    }
    trusted_directory(&journals)?;
    // Upgrade earlier helper journals without exposing process/identity metadata.
    fs::set_permissions(&journals, fs::Permissions::from_mode(0o700))?;
    sync_dir(&journals)?;
    let path = journals.join(format!("{id}.json"));
    let missing_journal = match fs::symlink_metadata(&path) {
        Ok(_) => false,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => return Err(error.into()),
    };
    if request.action == Action::Recover && missing_journal {
        // The controller may have persisted its intent before root preparation
        // reached the first journal write. Staging/switches are write-ahead, so
        // absent journal AND absent owned artifacts proves no activation to undo.
        return recover_unprepared(&id, sha);
    }
    if request.action == Action::Prepare && !path.exists() {
        // A different uncommitted transaction cannot own the same current link.
        for entry in fs::read_dir(&journals)? {
            let entry = entry?;
            if entry.path().extension().is_some_and(|ext| ext == "json") {
                let previous: Journal =
                    serde_json::from_reader(trusted_open(&entry.path(), false)?)?;
                if !matches!(previous.phase.as_str(), "committed" | "rolledBack") {
                    bail!("an unfinished Hand transaction exists; recover it first");
                }
            } else {
                bail!("foreign file in Hand update journal directory");
            }
        }
        let mut journal = prepare(&id, sha, &mut candidate_file, request.start_stopped, &path)?;
        journal.receipts.insert("prepare".into(), "prepared".into());
        save(&path, &journal)?;
    }
    let mut journal: Journal =
        serde_json::from_reader(trusted_open(&path, false).context("transaction not prepared")?)?;
    if journal.protocol != 1 || journal.transaction != id || journal.candidate_sha256 != sha {
        bail!("transaction identity or candidate SHA mismatch");
    }
    guard(&journal)?;
    match request.action {
        Action::Prepare => {
            if journal.phase == "preparing" {
                if snapshot(&journal.release)? != journal.new_files {
                    bail!(
                        "interrupted preparation is incomplete; recover abandons it without touching the Hand"
                    );
                }
                journal.candidate_complete = true;
                journal.phase = "prepared".into();
                journal.receipts.insert("prepare".into(), "prepared".into());
                save(&path, &journal)?;
                guard(&journal)?;
            }
        }
        Action::Apply => apply(&path, &mut journal)?,
        Action::Commit => {
            if journal.phase != "committed" {
                if journal.phase != "applied" {
                    bail!(
                        "commit requires an applied transaction and caller's independent account/publisher proof"
                    );
                }
                verify_running(&journal, sha)?;
                journal.phase = "committed".into();
                journal.receipts.insert("commit".into(), "committed".into());
                save(&path, &journal)?;
            }
        }
        Action::Rollback => rollback(&path, &mut journal)?,
        Action::Recover => {
            reconcile_pointer(&path, &mut journal)?;
            match journal.phase.as_str() {
                "preparing" => {
                    journal.phase = "rolledBack".into();
                    journal
                        .receipts
                        .insert("rollback".into(), "rolledBack".into());
                    save(&path, &journal)?;
                }
                "applying" | "restarting" => {
                    if verify_running(&journal, sha).is_ok()
                        && journal.current.target == journal.release
                    {
                        journal.phase = "applied".into();
                        journal.receipts.insert("apply".into(), "applied".into());
                        save(&path, &journal)?;
                    } else {
                        rollback(&path, &mut journal)?;
                    }
                }
                "rollingBack" => rollback(&path, &mut journal)?,
                "rollbackRestarting" | "rollbackUncertain" => finish_rollback(&path, &mut journal)?,
                "prepared" | "applied" | "committed" | "rolledBack" => {}
                _ => bail!("unknown journal phase; manual recovery required"),
            }
            journal
                .receipts
                .insert("recover".into(), journal.phase.clone());
            save(&path, &journal)?;
        }
        Action::Status | Action::Capabilities => unreachable!(),
    }
    receipt(&journal)
}

fn require_no_unjournaled_artifacts(root: &Path, id: &str) -> Result<()> {
    for artifact in [
        root.join("releases").join(format!("hand-update-{id}")),
        root.join(format!(".hand-update-{id}-apply")),
        root.join(format!(".hand-update-{id}-rollback")),
    ] {
        match fs::symlink_metadata(&artifact) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
            Ok(_) => bail!(
                "missing journal has owned release/pointer artifacts; manual recovery required"
            ),
        }
    }
    Ok(())
}
fn recover_unprepared(id: &str, sha: &str) -> Result<()> {
    require_no_unjournaled_artifacts(Path::new(ROOT), id)?;
    // Another controller/store may own the system service. Absence of our
    // journal does not grant authority to finalize or undo its handover.
    for entry in fs::read_dir(Path::new(ROOT).join("hand-updates"))? {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            bail!("foreign update journal artifact; inspect before recovery");
        }
        let other: Journal = serde_json::from_reader(trusted_open(&path, false)?)?;
        if !matches!(other.phase.as_str(), "committed" | "rolledBack") {
            bail!("another system Hand transaction remains unfinished");
        }
    }
    let current = pointer()?;
    let current_sha = release_hash(&current.target.join("nanocodex2"))?;
    let props = unit(HAND)?;
    validate_hand_args(&command_argv(&props)?)?;
    unit_layout(&props)?;
    let user = value(&props, "User");
    let uid = service_uid(user)?;
    let active = value(&props, "ActiveState") == "active";
    if active {
        verify_process(&props, &current_sha, uid)?;
    } else {
        verify_stopped()?;
    }
    // Inspect separation only; never restart/stop a Hand, factory or guest.
    factories(&props)?;
    require_no_unjournaled_artifacts(Path::new(ROOT), id)?;
    if pointer()? != current {
        bail!("current pointer changed during unprepared recovery");
    }
    println!(
        "{}",
        serde_json::json!({"protocol":1, "transaction":id,
        "candidate_sha256":sha, "phase":"rolledBack", "candidate_complete":false,
        "recovered_unprepared":true, "current_sha256":current_sha,
        "pid":pid(&props)?, "runtime_verified":active, "was_active":active})
    );
    Ok(())
}

use std::io::Seek as _;
fn valid_id(value: &str) -> Result<String> {
    let id = uuid::Uuid::parse_str(value).context("invalid transaction UUID")?;
    if id.is_nil() || id.to_string() != value {
        bail!("transaction must be a non-nil canonical UUID");
    }
    Ok(value.to_owned())
}
fn metadata_safe(metadata: &fs::Metadata, directory: bool) -> Result<()> {
    if metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || (directory && !metadata.is_dir())
        || (!directory && (!metadata.is_file() || metadata.nlink() != 1))
    {
        bail!(
            "expected root-owned non-writable {} without links",
            if directory {
                "directory"
            } else {
                "regular file"
            }
        );
    }
    Ok(())
}
fn trusted_directory(path: &Path) -> Result<()> {
    metadata_safe(
        &fs::symlink_metadata(path)
            .with_context(|| format!("unsafe/missing {}", path.display()))?,
        true,
    )
}
fn trusted_open(path: &Path, create: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(create)
        .create(create)
        .truncate(false)
        .mode(0o600)
        .custom_flags(NOFOLLOW)
        .open(path)?;
    metadata_safe(&file.metadata()?, false)
        .with_context(|| format!("unsafe file {}", path.display()))?;
    let on_disk = fs::symlink_metadata(path)?;
    if file.metadata()?.ino() != on_disk.ino() || file.metadata()?.dev() != on_disk.dev() {
        bail!("file changed during open");
    }
    Ok(file)
}
fn digest(file: &mut File) -> Result<String> {
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hex::encode(hash.finalize()))
}
fn release_metadata(path: &Path, meta: &fs::Metadata) -> Result<()> {
    release_metadata_under(&Path::new(ROOT).join("releases"), path, meta)
}
fn release_metadata_under(releases: &Path, path: &Path, meta: &fs::Metadata) -> Result<()> {
    if meta.uid() != 0 || meta.mode() & 0o7022 != 0 || !meta.is_file() {
        bail!("unsafe release file");
    }
    if meta.nlink() > 1 {
        fn count(dir: &Path, target: &fs::Metadata) -> Result<u64> {
            trusted_directory(dir)?;
            let mut found = 0;
            for child in fs::read_dir(dir)? {
                let path = child?.path();
                let meta = fs::symlink_metadata(&path)?;
                if meta.is_dir() {
                    found += count(&path, target)?;
                } else if meta.dev() == target.dev() && meta.ino() == target.ino() {
                    if !meta.is_file() || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
                        bail!("unsafe linked release companion");
                    }
                    found += 1;
                }
            }
            Ok(found)
        }
        if !path.starts_with(releases) || count(releases, meta)? != meta.nlink() {
            bail!("release file has foreign hard links");
        }
    }
    Ok(())
}
fn release_open(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(NOFOLLOW)
        .open(path)?;
    let meta = file.metadata()?;
    release_metadata(path, &meta)?;
    let disk = fs::symlink_metadata(path)?;
    if meta.dev() != disk.dev() || meta.ino() != disk.ino() {
        bail!("release file changed during open");
    }
    Ok(file)
}
fn release_hash(path: &Path) -> Result<String> {
    digest(&mut release_open(path)?)
}
fn validate_elf(file: &mut File) -> Result<()> {
    let mut header = [0u8; 20];
    file.read_exact(&mut header)
        .context("candidate is not a Linux executable")?;
    let machine = if cfg!(target_arch = "x86_64") {
        62u16
    } else if cfg!(target_arch = "aarch64") {
        183
    } else {
        bail!("unsupported Linux architecture");
    };
    if &header[..4] != b"\x7fELF"
        || header[4] != 2
        || header[5] != 1
        || !matches!(header[7], 0 | 3)
        || u16::from_le_bytes([header[18], header[19]]) != machine
        || !matches!(u16::from_le_bytes([header[16], header[17]]), 2 | 3)
    {
        bail!(
            "candidate runtime check failed: expected architecture-matching Linux ELF executable"
        );
    }
    Ok(())
}
fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}
fn pointer() -> Result<Pointer> {
    let path = Path::new(ROOT).join("current");
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_symlink() || metadata.uid() != 0 || metadata.nlink() != 1 {
        bail!("current must be the root-owned installation symlink");
    }
    let raw = fs::read_link(&path)?;
    let target = if raw.is_absolute() {
        raw
    } else {
        Path::new(ROOT).join(raw)
    };
    if target.parent() != Some(Path::new(ROOT).join("releases").as_path())
        || target
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        bail!("current points outside the exact release directory");
    }
    trusted_directory(&target)?;
    Ok(Pointer {
        target,
        dev: metadata.dev(),
        ino: metadata.ino(),
    })
}
fn snapshot(root: &Path) -> Result<BTreeMap<String, Entry>> {
    fn walk(root: &Path, dir: &Path, entries: &mut BTreeMap<String, Entry>) -> Result<()> {
        trusted_directory(dir)?;
        for child in fs::read_dir(dir)? {
            let path = child?.path();
            let name = path
                .strip_prefix(root)?
                .to_str()
                .context("non-UTF8 release filename")?
                .to_owned();
            let meta = fs::symlink_metadata(&path)?;
            if meta.is_dir() {
                metadata_safe(&meta, true)?;
                entries.insert(
                    name,
                    Entry {
                        directory: true,
                        mode: meta.mode() & 0o777,
                        sha256: String::new(),
                    },
                );
                walk(root, &path, entries)?;
            } else {
                release_metadata(&path, &meta)?;
                entries.insert(
                    name,
                    Entry {
                        directory: false,
                        mode: meta.mode() & 0o777,
                        sha256: release_hash(&path)?,
                    },
                );
            }
        }
        Ok(())
    }
    let mut entries = BTreeMap::new();
    walk(root, root, &mut entries)?;
    Ok(entries)
}
fn save(path: &Path, journal: &Journal) -> Result<()> {
    let parent = path.parent().context("journal parent")?;
    trusted_directory(parent)?;
    if path.exists() {
        let _ = trusted_open(path, false)?;
    }
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    serde_json::to_writer(&mut temp, journal)?;
    temp.as_file().sync_all()?;
    temp.persist(path)?;
    sync_dir(parent)
}

const PROPERTIES: &[&str] = &[
    "Id",
    "LoadState",
    "ActiveState",
    "SubState",
    "MainPID",
    "User",
    "ExecStart",
    "ExecStartPre",
    "ExecStartPost",
    "ExecStop",
    "ExecStopPost",
    "ExecReload",
    "ExecCondition",
    "FragmentPath",
    "DropInPaths",
    "ControlGroup",
    "Requires",
    "Wants",
    "BindsTo",
    "PartOf",
    "ConsistsOf",
    "RequiredBy",
    "BoundBy",
    "PropagatesStopTo",
    "StopPropagatedFrom",
    "Triggers",
    "TriggeredBy",
    "KillMode",
    "RootDirectory",
    "RootImage",
    "PrivateUsers",
    "DynamicUser",
    "Type",
];
fn systemctl(args: &[&str]) -> Result<String> {
    // No PATH lookup, shell, arbitrary executable, or command supplied by JSON.
    trusted_directory(Path::new("/usr"))?;
    trusted_directory(Path::new("/usr/bin"))?;
    let _ = trusted_open(Path::new("/usr/bin/systemctl"), false)?;
    let output = Command::new("/usr/bin/systemctl")
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .output()?;
    if !output.status.success() {
        bail!(
            "systemctl operation failed ({})",
            args.first().unwrap_or(&"unknown")
        );
    }
    String::from_utf8(output.stdout).context("systemctl returned invalid text")
}
fn unit(name: &str) -> Result<BTreeMap<String, String>> {
    let properties = format!("--property={}", PROPERTIES.join(","));
    let text = systemctl(&["show", &properties, "--no-pager", "--", name])?;
    Ok(text
        .lines()
        .filter_map(|line| {
            line.split_once('=')
                .map(|(a, b)| (a.to_owned(), b.to_owned()))
        })
        .collect())
}
fn value<'a>(props: &'a BTreeMap<String, String>, key: &str) -> &'a str {
    props.get(key).map(String::as_str).unwrap_or("")
}
fn pid(props: &BTreeMap<String, String>) -> Result<u32> {
    value(props, "MainPID")
        .parse()
        .context("invalid systemd MainPID")
}
fn service_uid(user: &str) -> Result<u32> {
    if user.is_empty()
        || !user
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
    {
        bail!("expected an explicit persistent service user");
    }
    let passwd = fs::read_to_string("/etc/passwd")?;
    let uid = passwd
        .lines()
        .find_map(|line| {
            let parts: Vec<_> = line.split(':').collect();
            if parts.len() >= 4 && parts[0] == user {
                parts[2].parse::<u32>().ok()
            } else {
                None
            }
        })
        .context("service user not found in local passwd; NSS/dynamic users are unsupported")?;
    if uid == 0 {
        bail!("Hand service must use a non-root account");
    }
    Ok(uid)
}
fn command_argv(props: &BTreeMap<String, String>) -> Result<Vec<String>> {
    let start = value(props, "ExecStart");
    if !start.starts_with("{ path=") || start.matches("{ path=").count() != 1 {
        bail!("unknown/multiple systemd executable layout");
    }
    let executable = start
        .strip_prefix("{ path=")
        .and_then(|v| v.split_once(" ; ").map(|(path, _)| path))
        .context("unknown systemd executable")?;
    let args = start
        .split_once(" ; argv[]=")
        .and_then(|(_, s)| s.split_once(" ; ").map(|(v, _)| v))
        .context("unknown systemd ExecStart layout")?;
    let args = shlex::split(args).context("invalid systemd ExecStart arguments")?;
    if args.first().map(String::as_str) != Some(executable) {
        bail!("systemd argv/executable mismatch");
    }
    Ok(args)
}
fn unit_layout(props: &BTreeMap<String, String>) -> Result<BTreeMap<String, String>> {
    if value(props, "LoadState") != "loaded"
        || !matches!(value(props, "Type"), "simple" | "exec" | "notify")
        || !matches!(value(props, "KillMode"), "mixed" | "control-group")
        || value(props, "DynamicUser") != "no"
        || value(props, "PrivateUsers") != "no"
    {
        bail!("unsupported unit ownership, service type or kill layout");
    }
    for key in [
        "ExecStartPre",
        "ExecStartPost",
        "ExecStop",
        "ExecStopPost",
        "ExecReload",
        "ExecCondition",
        "RootDirectory",
        "RootImage",
    ] {
        if !value(props, key).is_empty() {
            bail!("unit has unsupported hooks or alternate root");
        }
    }
    let mut layout = props.clone();
    for key in [
        "ActiveState",
        "SubState",
        "MainPID",
        "ControlGroup",
        "ExecStart",
    ] {
        layout.remove(key);
    }
    layout.insert("argv".into(), serde_json::to_string(&command_argv(props)?)?);
    pin_unit_files(props, &mut layout)?;
    Ok(layout)
}
fn pin_unit_files(
    props: &BTreeMap<String, String>,
    layout: &mut BTreeMap<String, String>,
) -> Result<()> {
    // Never read the unit's Environment or account credential contents. Pin the
    // root-owned unit/drop-in inodes and metadata instead of copying their text.
    let paths = std::iter::once(value(props, "FragmentPath"))
        .chain(value(props, "DropInPaths").split_whitespace());
    for path in paths {
        if path.is_empty() {
            bail!("missing unit fragment");
        }
        let path = Path::new(path);
        if !path.starts_with("/etc/systemd/system")
            && !path.starts_with("/usr/lib/systemd/system")
            && !path.starts_with("/lib/systemd/system")
        {
            bail!("foreign unit fragment/drop-in path");
        }
        // /lib may be a distro compatibility link; resolve only this fixed base.
        let canonical = path.canonicalize()?;
        let parent = canonical.parent().context("unit parent")?;
        let mut ancestors: Vec<_> = parent.ancestors().collect();
        ancestors.reverse();
        for ancestor in ancestors {
            trusted_directory(ancestor)?;
        }
        if fs::symlink_metadata(path)?.file_type().is_symlink() {
            bail!("symlinked unit/drop-in is unsupported");
        }
        let file = trusted_open(&canonical, false)?;
        let m = file.metadata()?;
        layout.insert(
            format!("file:{}", canonical.display()),
            format!(
                "{}:{}:{}:{}:{}:{}",
                m.dev(),
                m.ino(),
                m.len(),
                m.mtime(),
                m.mtime_nsec(),
                m.mode()
            ),
        );
    }
    Ok(())
}
fn cgroup(props: &BTreeMap<String, String>) -> Result<String> {
    let group = value(props, "ControlGroup");
    if !group.starts_with('/') || group == "/" || group.split('/').any(|c| c == ".." || c == ".") {
        bail!("missing/unsafe systemd cgroup");
    }
    if !Path::new("/sys/fs/cgroup/cgroup.controllers").exists() {
        bail!("cgroup v2 is required to prove factory separation");
    }
    Ok(group.to_owned())
}
fn process_pin(pid: u32) -> Result<ProcessPin> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let rest = stat.rsplit_once(") ").context("invalid proc stat")?.1;
    let start = rest
        .split_whitespace()
        .nth(19)
        .context("missing process start tick")?
        .to_owned();
    let status = fs::read_to_string(format!("/proc/{pid}/status"))?;
    let ids: Vec<u32> = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .context("missing process UID")?
        .split_whitespace()
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()?;
    if ids.len() != 4 || ids.iter().any(|id| *id != ids[0]) {
        bail!("unexpected process UID credentials");
    }
    Ok(ProcessPin {
        pid,
        start,
        uid: ids[0],
    })
}
fn pins_unchanged(pins: &[ProcessPin]) -> Result<()> {
    for pin in pins {
        let now = process_pin(pin.pid).context("a retained factory/guest process exited")?;
        if now.start != pin.start || now.uid != pin.uid {
            bail!("retained factory/guest PID changed identity");
        }
    }
    Ok(())
}
fn in_cgroup(pid: u32, group: &str) -> Result<bool> {
    let text = fs::read_to_string(format!("/proc/{pid}/cgroup"))?;
    let actual = text
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .context("process has unknown cgroup layout")?;
    Ok(actual == group
        || actual
            .strip_prefix(group)
            .is_some_and(|tail| tail.starts_with('/')))
}
fn cgroup_pids(group: &str) -> Result<Vec<u32>> {
    fn collect(dir: &Path, pids: &mut Vec<u32>) -> Result<()> {
        for line in fs::read_to_string(dir.join("cgroup.procs"))?.lines() {
            pids.push(line.parse()?);
        }
        for child in fs::read_dir(dir)? {
            let child = child?;
            if child.file_type()?.is_dir() {
                collect(&child.path(), pids)?;
            }
        }
        Ok(())
    }
    let mut pids = Vec::new();
    collect(
        &Path::new("/sys/fs/cgroup").join(group.trim_start_matches('/')),
        &mut pids,
    )?;
    pids.sort_unstable();
    pids.dedup();
    Ok(pids)
}
fn dependent(a: &BTreeMap<String, String>, unit: &str) -> bool {
    [
        "Requires",
        "Wants",
        "BindsTo",
        "PartOf",
        "ConsistsOf",
        "RequiredBy",
        "BoundBy",
        "PropagatesStopTo",
        "StopPropagatedFrom",
        "Triggers",
        "TriggeredBy",
    ]
    .iter()
    .any(|key| value(a, key).split_whitespace().any(|v| v == unit))
}
// This is a bounded exception, not a name-prefix exemption. The existing
// Windows VM's UDP forwarding service/timer are unrelated to the Hand runtime.
// Revalidate all evidence on every guard; never execute or change either unit.
fn ancillary_layout(props: &BTreeMap<String, String>) -> Result<()> {
    if value(props, "Id") != ANCILLARY
        || value(props, "LoadState") != "loaded"
        || value(props, "ActiveState") != "inactive"
        || value(props, "SubState") != "dead"
        || pid(props)? != 0
        || !matches!(value(props, "Type"), "oneshot" | "simple" | "exec")
        || !matches!(value(props, "User"), "" | "root" | "0")
        || value(props, "DynamicUser") != "no"
        || value(props, "PrivateUsers") != "no"
        || command_argv(props)? != [ANCILLARY_EXE, "apply"]
    {
        bail!("unknown ancillary Nanocodex service layout");
    }
    for key in [
        "ExecStartPre",
        "ExecStartPost",
        "ExecStop",
        "ExecStopPost",
        "ExecReload",
        "ExecCondition",
        "RootDirectory",
        "RootImage",
        "BindsTo",
        "PartOf",
    ] {
        if !value(props, key).is_empty() {
            bail!("ancillary service has hooks, alternate root or lifecycle coupling");
        }
    }
    if props.values().any(|v| v.contains(ROOT)) {
        bail!("ancillary service references the Hand installation");
    }
    Ok(())
}
fn ancillary_binary(path: &Path) -> Result<()> {
    // Reject aliases/symlinks (including ancestors) into current or a release.
    if path.canonicalize()? != path {
        bail!("ancillary executable must be a canonical independent path");
    }
    for ancestor in path
        .parent()
        .context("ancillary executable parent")?
        .ancestors()
    {
        trusted_directory(ancestor)?;
    }
    let file = trusted_open(path, false)?;
    if file.metadata()?.mode() & 0o111 == 0 {
        bail!("ancillary executable is not executable");
    }
    Ok(())
}
fn verify_ancillary(
    props: &BTreeMap<String, String>,
    hand: &BTreeMap<String, String>,
) -> Result<()> {
    ancillary_layout(props)?;
    ancillary_binary(Path::new(ANCILLARY_EXE))?;
    pin_unit_files(props, &mut BTreeMap::new())?;
    let timer = "nanocodex-win11-webrtc.timer";
    // Timer-triggered forwarding is allowed, but neither direction may share
    // the Hand's start/stop lifecycle, directly or through intermediate units.
    for name in [ANCILLARY, timer] {
        let props = if name == ANCILLARY {
            props.clone()
        } else {
            unit(name)?
        };
        if name == timer && value(&props, "LoadState") == "not-found" {
            continue;
        }
        if name == timer {
            if value(&props, "LoadState") != "loaded"
                || value(&props, "Id") != timer
                || props.values().any(|v| v.contains(ROOT))
            {
                bail!("unknown ancillary timer layout");
            }
            pin_unit_files(&props, &mut BTreeMap::new())?;
        }
        if dependent(hand, name)
            || dependent(&props, HAND)
            || lifecycle_reaches(HAND, name)?
            || lifecycle_reaches(name, HAND)?
        {
            bail!("Hand/ancillary systemd dependencies are coupled");
        }
    }
    Ok(())
}
fn runtime_executable(path: &Path, resolved: Option<&Path>) -> bool {
    matches!(
        path.file_name().and_then(|v| v.to_str()),
        Some("nanocodex2" | "nanocodex")
    ) || path.starts_with(ROOT)
        || resolved.is_some_and(|p| p.starts_with(ROOT))
}
fn runtime_unit(name: &str, props: &BTreeMap<String, String>) -> bool {
    name.starts_with("nanocodex")
        || props.values().any(|v| v.contains(ROOT))
        // Inspect every command path, including hooks and multi-command units.
        // Aliases must not evade discovery merely by using a non-Nanocodex name.
        || props.iter().filter(|(key, _)| key.starts_with("Exec")).any(|(_, commands)| {
            commands.split("{ path=").skip(1).any(|command| {
                command.split_once(" ; ").is_some_and(|(path, _)| {
                    let path = Path::new(path);
                    runtime_executable(path, path.canonicalize().ok().as_deref())
                })
            })
        })
}
fn service_units() -> Result<Vec<String>> {
    let text = systemctl(&[
        "list-units",
        "--all",
        "--type=service",
        "--no-legend",
        "--plain",
        "--no-pager",
    ])?;
    Ok(text
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| name.ends_with(".service"))
        .map(str::to_owned)
        .collect())
}
fn factories(hand: &BTreeMap<String, String>) -> Result<Vec<Factory>> {
    let mut result = Vec::new();
    let names = service_units()?;
    for name in names.iter().map(String::as_str) {
        if name == HAND {
            continue;
        }
        let props = unit(name)?;
        if name == ANCILLARY {
            verify_ancillary(&props, hand)?;
            continue;
        }
        if !runtime_unit(name, &props) {
            continue;
        }
        if !matches!(
            name,
            "nanocodex-factory.service" | "nanocodex-vm-factory.service" | "nanocodex-host.service"
        ) {
            bail!("unknown/coupled Nanocodex service layout");
        }
        let args = command_argv(&props)?;
        if args.get(1).map(String::as_str) != Some("host")
            || args
                .iter()
                .any(|v| matches!(v.as_str(), "--api-key" | "--credential" | "--parent-pipe"))
        {
            bail!("retained factory must have a separate known host executable unit");
        }
        let active_state = value(&props, "ActiveState").to_owned();
        let main = pid(&props)?;
        // Existing active factory units may name current, but the kernel already
        // pins their older executable. Pin that release path AND inode, never the
        // Hand's mutable current path. No factory lifecycle action is permitted.
        let configured = Path::new(args.first().context("factory executable missing")?);
        let binary = if active_state == "active" && main != 0 {
            fs::read_link(format!("/proc/{main}/exe"))?
        } else if active_state == "inactive" && main == 0 {
            if configured.starts_with(Path::new(ROOT).join("current")) {
                bail!("inactive factory executable follows the mutable Hand current pointer");
            }
            configured.to_owned()
        } else {
            bail!("retained factory has unknown transitional state");
        };
        validate_factory_binary(configured, &binary)?;
        let mut executable = release_open(&binary)?;
        let executable_meta = executable.metadata()?;
        let executable_sha256 = digest(&mut executable)?;
        if dependent(hand, name)
            || dependent(&props, HAND)
            || lifecycle_reaches(HAND, name)?
            || lifecycle_reaches(name, HAND)?
        {
            bail!("Hand/factory systemd dependencies are coupled");
        }
        let layout = unit_layout(&props)?;
        if active_state == "inactive" && main == 0 {
            result.push(Factory {
                unit: name.into(),
                cgroup: String::new(),
                main,
                pins: Vec::new(),
                layout,
                active_state,
                executable_sha256,
                executable: binary,
                executable_dev: executable_meta.dev(),
                executable_ino: executable_meta.ino(),
            });
            continue;
        }
        if active_state != "active" {
            bail!("retained factory has unknown transitional state");
        }
        let group = cgroup(&props)?;
        let hand_group = value(hand, "ControlGroup");
        if group == hand_group
            || group
                .strip_prefix(hand_group)
                .is_some_and(|tail| !hand_group.is_empty() && tail.starts_with('/'))
            || hand_group
                .strip_prefix(&group)
                .is_some_and(|tail| tail.starts_with('/'))
        {
            bail!("Hand/factory cgroups are not separate");
        }
        if main == 0 || !in_cgroup(main, &group)? {
            bail!("factory MainPID is not owned by its separate cgroup");
        }
        verify_factory_process(
            main,
            &binary,
            executable_meta.dev(),
            executable_meta.ino(),
            &executable_sha256,
        )?;
        let mut pids = cgroup_pids(&group)?;
        if !pids.contains(&main) {
            bail!("factory MainPID disappeared from cgroup snapshot");
        }
        // Include all descendants even if a factory has moved them into a scope.
        let mut proc_parents = Vec::new();
        for entry in fs::read_dir("/proc")? {
            let entry = entry?;
            if let Ok(p) = entry.file_name().to_string_lossy().parse::<u32>()
                && let Ok(stat) = fs::read_to_string(entry.path().join("stat"))
                && let Some((_, rest)) = stat.rsplit_once(") ")
                && let Some(parent) = rest
                    .split_whitespace()
                    .nth(1)
                    .and_then(|v| v.parse::<u32>().ok())
            {
                proc_parents.push((p, parent));
            }
        }
        loop {
            let before = pids.len();
            for (child, parent) in &proc_parents {
                if pids.contains(parent) && !pids.contains(child) {
                    pids.push(*child);
                }
            }
            if pids.len() == before {
                break;
            }
        }
        let pins = pids
            .into_iter()
            .map(process_pin)
            .collect::<Result<Vec<_>>>()?;
        for pin in &pins {
            if !hand_group.is_empty() && in_cgroup(pin.pid, hand_group)? {
                bail!("factory child is in Hand cgroup");
            }
        }
        pins_unchanged(&pins)?;
        result.push(Factory {
            unit: name.into(),
            cgroup: group,
            main,
            pins,
            layout,
            active_state,
            executable_sha256,
            executable: binary,
            executable_dev: executable_meta.dev(),
            executable_ino: executable_meta.ino(),
        });
    }
    Ok(result)
}

fn prepare(
    id: &str,
    sha: &str,
    candidate: &mut File,
    start_stopped: bool,
    path: &Path,
) -> Result<Journal> {
    let original = pointer()?;
    let old_files = snapshot(&original.target)?;
    let old_binary = old_files
        .get("nanocodex2")
        .context("active release lacks nanocodex2")?;
    if old_binary.directory || old_binary.mode & 0o111 == 0 {
        bail!("active release binary is not executable");
    }
    let props = unit(HAND)?;
    let args = command_argv(&props)?;
    validate_hand_args(&args)?;
    let layout = unit_layout(&props)?;
    let service_user = value(&props, "User").to_owned();
    let service_uid = service_uid(&service_user)?;
    let was_active =
        value(&props, "ActiveState") == "active" && value(&props, "SubState") == "running";
    if !was_active && !(value(&props, "ActiveState") == "inactive" && pid(&props)? == 0) {
        bail!("Hand has an unknown transitional/failed service state");
    }
    let (state_directory, machine_id) =
        identity_state(&args, &service_user, service_uid, pid(&props)?)?;
    let retained = factories(&props)?;
    let group = if was_active {
        verify_process(&props, &old_binary.sha256, service_uid)?;
        cgroup(&props)?
    } else {
        String::new()
    };
    // Catch an integrated factory or an otherwise unowned root-release process.
    // Do not read process environment/account files or print command contents.
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let Ok(p) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if p == std::process::id() {
            continue;
        }
        if let Ok(exe) = fs::read_link(entry.path().join("exe"))
            && exe.starts_with(ROOT)
        {
            if !group.is_empty() && in_cgroup(p, &group)? {
                let cmd = fs::read(entry.path().join("cmdline"))?;
                if cmd
                    .split(|v| *v == 0)
                    .any(|arg| arg == b"host" || arg == b"--vm")
                {
                    bail!("factory/VM process shares Hand lifecycle; independent Hand required");
                }
            } else if !retained
                .iter()
                .any(|factory| factory.pins.iter().any(|pin| pin.pid == p))
            {
                bail!("unknown Nanocodex release process outside Hand/factory ownership");
            }
        }
    }
    let release = Path::new(ROOT)
        .join("releases")
        .join(format!("hand-update-{id}"));
    let mut expected = old_files.clone();
    expected.insert(
        "nanocodex2".into(),
        Entry {
            directory: false,
            mode: 0o755,
            sha256: sha.into(),
        },
    );
    let source_revision = format!("{}\n", crate::version::git_sha().unwrap_or("unknown"));
    if let Some(previous) = old_files.get("source-revision") {
        if previous.directory {
            bail!("source-revision metadata must be a regular file");
        }
        expected.insert(
            "source-revision".into(),
            Entry {
                directory: false,
                mode: previous.mode,
                sha256: hex::encode(Sha256::digest(source_revision.as_bytes())),
            },
        );
    }
    let mut journal = Journal {
        protocol: 1,
        transaction: id.into(),
        candidate_sha256: sha.into(),
        phase: "preparing".into(),
        candidate_complete: false,
        was_active,
        start_stopped,
        original: original.clone(),
        current: original.clone(),
        pending: None,
        pending_path: None,
        old_files: old_files.clone(),
        new_files: expected.clone(),
        release: release.clone(),
        service_user,
        service_uid,
        state_directory,
        machine_id,
        apply_started_millis: 0,
        layout,
        factories: retained,
        receipts: BTreeMap::new(),
    };
    if release.exists() {
        bail!("foreign transaction release exists before prepare");
    }
    save(path, &journal)?; // durable preparation intent before any staging writes
    fs::create_dir(&release).context("transaction release already exists; refusing adoption")?;
    fs::set_permissions(&release, fs::Permissions::from_mode(0o755))?;
    for (name, entry) in &old_files {
        let destination = release.join(name);
        if entry.directory {
            fs::create_dir(&destination)?;
            fs::set_permissions(&destination, fs::Permissions::from_mode(entry.mode))?;
        } else {
            let mut source = release_open(&original.target.join(name))?;
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(NOFOLLOW)
                .open(&destination)?;
            if name == "nanocodex2" {
                candidate.rewind()?;
                std::io::copy(candidate, &mut output)?;
            } else if name == "source-revision" {
                output.write_all(source_revision.as_bytes())?;
            } else {
                std::io::copy(&mut source, &mut output)?;
            }
            output.set_permissions(fs::Permissions::from_mode(if name == "nanocodex2" {
                0o755
            } else {
                entry.mode
            }))?;
            output.sync_all()?;
        }
    }
    for (name, entry) in old_files.iter().rev() {
        if entry.directory {
            sync_dir(&release.join(name))?;
        }
    }
    sync_dir(&release)?;
    sync_dir(&Path::new(ROOT).join("releases"))?;
    if snapshot(&original.target)? != old_files
        || snapshot(&release)? != expected
        || pointer()? != original
    {
        bail!("release/pointer changed during prepare");
    }
    journal.candidate_complete = true;
    journal.phase = "prepared".into();
    guard(&journal)?;
    Ok(journal)
}
fn validate_hand_args(args: &[String]) -> Result<()> {
    // Units are written with EXE; the same binary is also released as nanocodex.
    if !matches!(
        args.first().map(String::as_str),
        Some(EXE | "/opt/nanocodex/current/nanocodex")
    ) {
        bail!("unknown Hand executable");
    }
    if args.get(1).map(String::as_str) == Some("__device-hand")
        && args.len() == 3
        && args[2] == "--daemon"
    {
        return Ok(());
    }
    if args.get(1).map(String::as_str) != Some("hand") {
        bail!("expected an independent persistent Hand daemon (no parent pipe)");
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut remaining = &args[2..];
    while !remaining.is_empty() {
        if remaining.len() < 2 || !seen.insert(remaining[0].as_str()) {
            bail!("duplicate or incomplete Hand command options");
        }
        let option = remaining[0].as_str();
        let value = remaining[1].as_str();
        match option {
            "--state-dir" | "--workspace" => {
                if !Path::new(value).is_absolute()
                    || Path::new(value)
                        .components()
                        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
                {
                    bail!("Hand workspace/state path must be a clean absolute path");
                }
            }
            "--machine-name" => {
                if value.is_empty() || value.starts_with('-') || value.chars().any(char::is_control)
                {
                    bail!("invalid Hand machine name");
                }
            }
            "--log-format" if matches!(value, "json" | "text") => {}
            // This advertises an independently managed provider; it does not
            // start a host/VM. Runtime cgroup/process guards still reject hosts.
            "--vm-provider"
                if !value.is_empty()
                    && !value.starts_with('-')
                    && value
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.')) => {}
            _ => {
                bail!("unknown/coupled Hand command options; factory/VM migration is not an update")
            }
        }
        remaining = &remaining[2..];
    }
    Ok(())
}
fn guard(journal: &Journal) -> Result<()> {
    trusted_directory(Path::new(ROOT))?;
    trusted_directory(&Path::new(ROOT).join("releases"))?;
    if journal.release
        != Path::new(ROOT)
            .join("releases")
            .join(format!("hand-update-{}", journal.transaction))
        || journal.original.target.parent() != Some(Path::new(ROOT).join("releases").as_path())
    {
        bail!("journal release paths invalid");
    }
    if snapshot(&journal.original.target)? != journal.old_files {
        bail!("original release hash guard failed");
    }
    if journal.candidate_complete {
        for (name, entry) in &journal.new_files {
            if !entry.directory && fs::symlink_metadata(journal.release.join(name))?.nlink() != 1 {
                bail!("candidate inode has foreign/hard links");
            }
        }
        if snapshot(&journal.release)? != journal.new_files {
            bail!("candidate release hash guard failed");
        }
    } else if !matches!(journal.phase.as_str(), "preparing" | "rolledBack")
        || journal.current != journal.original
        || journal.pending.is_some()
    {
        bail!("incomplete candidate cannot be activated");
    }
    let actual = pointer()?;
    if actual != journal.current && journal.pending.as_ref() != Some(&actual) {
        bail!("current pointer guard failed; refusing foreign activation/rollback");
    }
    let props = unit(HAND)?;
    if unit_layout(&props)? != journal.layout
        || service_uid(value(&props, "User"))? != journal.service_uid
    {
        bail!("Hand unit/drop-ins/service user changed since prepare");
    }
    if service_units()?.iter().any(|name| name == ANCILLARY) {
        verify_ancillary(&unit(ANCILLARY)?, &props)?;
    }
    for factory in &journal.factories {
        let props = unit(&factory.unit)?;
        if value(&props, "ActiveState") != factory.active_state
            || pid(&props)? != factory.main
            || (factory.main != 0 && cgroup(&props)? != factory.cgroup)
            || unit_layout(&props)? != factory.layout
            || dependent(&props, HAND)
            || dependent(&unit(HAND)?, &factory.unit)
        {
            bail!("factory layout/lifecycle changed during Hand update");
        }
        let mut binary = release_open(&factory.executable)?;
        let meta = binary.metadata()?;
        if meta.dev() != factory.executable_dev
            || meta.ino() != factory.executable_ino
            || digest(&mut binary)? != factory.executable_sha256
        {
            bail!("independently pinned factory binary changed");
        }
        if factory.main != 0 {
            verify_factory_process(
                factory.main,
                &factory.executable,
                factory.executable_dev,
                factory.executable_ino,
                &factory.executable_sha256,
            )?;
        }
        if lifecycle_reaches(HAND, &factory.unit)? || lifecycle_reaches(&factory.unit, HAND)? {
            bail!("Hand/factory transitive systemd lifecycle coupling detected");
        }
        pins_unchanged(&factory.pins)?;
    }
    Ok(())
}
fn reconcile_pointer(path: &Path, journal: &mut Journal) -> Result<()> {
    guard(journal)?;
    if let Some(pending) = journal.pending.clone() {
        let temporary = journal
            .pending_path
            .clone()
            .context("missing pending pointer path")?;
        let apply = Path::new(ROOT).join(format!(".hand-update-{}-apply", journal.transaction));
        let rollback =
            Path::new(ROOT).join(format!(".hand-update-{}-rollback", journal.transaction));
        if temporary != apply && temporary != rollback {
            bail!("invalid pending pointer path");
        }
        if pointer()? != pending {
            let meta = fs::symlink_metadata(&temporary)?;
            if !meta.file_type().is_symlink()
                || meta.uid() != 0
                || meta.dev() != pending.dev
                || meta.ino() != pending.ino
                || fs::read_link(&temporary)? != pending.target
            {
                bail!("pending pointer inode guard failed");
            }
            guard(journal)?;
            fs::rename(&temporary, Path::new(ROOT).join("current"))?;
            sync_dir(Path::new(ROOT))?;
        }
        journal.current = pending;
        journal.pending = None;
        journal.pending_path = None;
        save(path, journal)?;
    }
    Ok(())
}
fn switch(path: &Path, journal: &mut Journal, target: &Path, suffix: &str) -> Result<()> {
    reconcile_pointer(path, journal)?;
    if journal.current.target == target {
        return Ok(());
    }
    guard(journal)?;
    let temporary = Path::new(ROOT).join(format!(".hand-update-{}-{suffix}", journal.transaction));
    symlink(target, &temporary).context("foreign pending pointer exists")?;
    sync_dir(Path::new(ROOT))?;
    let meta = fs::symlink_metadata(&temporary)?;
    journal.pending = Some(Pointer {
        target: target.to_owned(),
        dev: meta.dev(),
        ino: meta.ino(),
    });
    journal.pending_path = Some(temporary);
    save(path, journal)?; // write-ahead intent before the atomic link switch
    reconcile_pointer(path, journal)
}
fn verify_process(props: &BTreeMap<String, String>, sha: &str, uid: u32) -> Result<()> {
    if value(props, "ActiveState") != "active" || value(props, "SubState") != "running" {
        bail!("Hand is not active/running");
    }
    let p = pid(props)?;
    if p == 0 {
        bail!("Hand has no MainPID");
    }
    let before = process_pin(p)?;
    if before.uid != uid || !in_cgroup(p, &cgroup(props)?)? {
        bail!("Hand MainPID UID/cgroup mismatch");
    }
    // /proc/PID/exe is the kernel's pinned executable, not the current symlink.
    let mut file = File::open(format!("/proc/{p}/exe"))?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
        bail!("runtime executable inode is unsafe");
    }
    if digest(&mut file)? != sha {
        bail!("Hand runtime process SHA256 does not match the selected release");
    }
    let after = process_pin(p)?;
    if before.start != after.start || before.uid != after.uid || pid(&unit(HAND)?)? != p {
        bail!("Hand MainPID changed during exact executable verification");
    }
    Ok(())
}
fn verify_running(journal: &Journal, sha: &str) -> Result<()> {
    guard(journal)?;
    let original_sha = &journal
        .old_files
        .get("nanocodex2")
        .context("original executable missing")?
        .sha256;
    let should_run = if journal.current.target == journal.original.target && sha == original_sha {
        journal.was_active
    } else {
        journal.was_active || journal.start_stopped
    };
    if should_run {
        verify_process(&unit(HAND)?, sha, journal.service_uid)?;
        if journal.current.target == journal.release {
            readiness(journal)?;
        }
        Ok(())
    } else {
        verify_stopped()
    }
}
fn verify_stopped() -> Result<()> {
    let props = unit(HAND)?;
    if value(&props, "ActiveState") != "inactive" || pid(&props)? != 0 {
        bail!("originally stopped Hand lifecycle changed");
    }
    Ok(())
}
fn wait_running(journal: &Journal, sha: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        guard(journal)?; // changed retained guests/unit/pointer never gets retried
        match verify_running(journal, sha) {
            Ok(()) => return Ok(()),
            Err(error) if Instant::now() >= deadline => {
                return Err(error).context("Hand runtime check timed out");
            }
            Err(_) => std::thread::sleep(Duration::from_millis(300)),
        }
    }
}
fn apply(path: &Path, journal: &mut Journal) -> Result<()> {
    if matches!(journal.phase.as_str(), "applied" | "committed") {
        verify_running(journal, &journal.candidate_sha256)?;
        return Ok(());
    }
    if matches!(journal.phase.as_str(), "applying" | "restarting") {
        // An uncertain restart is reconciled, never blindly repeated.
        reconcile_pointer(path, journal)?;
        if journal.current.target == journal.release
            && verify_running(journal, &journal.candidate_sha256).is_ok()
        {
            journal.phase = "applied".into();
            journal.receipts.insert("apply".into(), "applied".into());
            save(path, journal)?;
            return Ok(());
        }
        rollback(path, journal)?;
        bail!("uncertain apply was rolled back; use a new transaction for another attempt");
    }
    if journal.phase != "prepared" {
        bail!("apply requires prepared phase");
    }
    journal.phase = "applying".into();
    save(path, journal)?;
    let target = journal.release.clone();
    switch(path, journal, &target, "apply")?;
    journal.apply_started_millis = epoch_millis()?;
    journal.phase = "restarting".into();
    save(path, journal)?;
    guard(journal)?;
    let attempt = if journal.was_active || journal.start_stopped {
        systemctl(&["restart", HAND]).and_then(|_| wait_running(journal, &journal.candidate_sha256))
    } else {
        verify_stopped()
    };
    if let Err(error) = attempt {
        match rollback(path, journal) {
            Ok(()) => {
                return Err(error)
                    .context("candidate runtime check failed; original Hand restored");
            }
            Err(rollback_error) => bail!(
                "candidate runtime check failed ({error:#}); rollback requires recovery ({rollback_error:#})"
            ),
        }
    }
    journal.phase = "applied".into();
    journal.receipts.insert("apply".into(), "applied".into());
    save(path, journal)
}
fn rollback(path: &Path, journal: &mut Journal) -> Result<()> {
    if journal.phase == "committed" {
        bail!("committed transaction cannot roll back");
    }
    if journal.phase == "rolledBack" {
        let sha = &journal
            .old_files
            .get("nanocodex2")
            .context("old executable missing")?
            .sha256;
        return verify_running(journal, sha);
    }
    if matches!(
        journal.phase.as_str(),
        "rollbackRestarting" | "rollbackUncertain"
    ) {
        return finish_rollback(path, journal);
    }
    reconcile_pointer(path, journal)?;
    if matches!(journal.phase.as_str(), "prepared" | "preparing") {
        journal.phase = "rolledBack".into();
        journal
            .receipts
            .insert("rollback".into(), "rolledBack".into());
        save(path, journal)?;
        return Ok(());
    }
    if journal.current.target == journal.original.target {
        let sha = &journal
            .old_files
            .get("nanocodex2")
            .context("old executable missing")?
            .sha256;
        if verify_running(journal, sha).is_ok() {
            journal.phase = "rolledBack".into();
            journal
                .receipts
                .insert("rollback".into(), "rolledBack".into());
            return save(path, journal);
        }
    }
    journal.phase = "rollingBack".into();
    save(path, journal)?;
    let original = journal.original.target.clone();
    switch(path, journal, &original, "rollback")?;
    journal.phase = "rollbackRestarting".into();
    save(path, journal)?;
    let sha = &journal
        .old_files
        .get("nanocodex2")
        .context("old executable missing")?
        .sha256;
    guard(journal)?;
    let result = if journal.was_active {
        systemctl(&["restart", HAND]).and_then(|_| wait_running(journal, sha))
    } else if journal.start_stopped {
        systemctl(&["stop", HAND]).and_then(|_| verify_stopped())
    } else {
        verify_stopped()
    };
    if let Err(error) = result {
        journal.phase = "rollbackUncertain".into();
        save(path, journal)?;
        return Err(error)
            .context("rollback restart uncertain; recover reconciles without blind restart");
    }
    finish_rollback(path, journal)
}
fn finish_rollback(path: &Path, journal: &mut Journal) -> Result<()> {
    reconcile_pointer(path, journal)?;
    if journal.current.target != journal.original.target {
        bail!("rollback pointer is not original");
    }
    let sha = &journal
        .old_files
        .get("nanocodex2")
        .context("old executable missing")?
        .sha256;
    verify_running(journal, sha)
        .context("rollback runtime uncertain; manual service repair required before recover")?;
    journal.phase = "rolledBack".into();
    journal
        .receipts
        .insert("rollback".into(), "rolledBack".into());
    save(path, journal)
}
fn receipt(journal: &Journal) -> Result<()> {
    let props = unit(HAND)?;
    let sha = release_hash(&journal.current.target.join("nanocodex2"))?;
    let verified = verify_process(&props, &sha, journal.service_uid).is_ok();
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "protocol": 1, "transaction": journal.transaction, "phase": journal.phase,
            "candidate_sha256": journal.candidate_sha256, "candidate_release": journal.release,
        "candidate_complete": journal.candidate_complete,
            "previous_release": journal.original.target, "was_active": journal.was_active,
            "current_sha256": sha, "service_user": journal.service_user,
            "pid": pid(&props)?, "runtime_verified": verified, "machine_id": journal.machine_id,
        }))?
    );
    Ok(())
}
fn status(transaction: Option<&str>) -> Result<()> {
    if !cfg!(target_os = "linux") {
        bail!("Linux systemd Hand status is unavailable on this platform");
    }
    // Fixed safe fields only, no environment/account data and no mutations.
    let props = unit(HAND)?;
    let current = pointer()?;
    let sha = release_hash(&current.target.join("nanocodex2"))?;
    let user = value(&props, "User");
    let verified = service_uid(user)
        .and_then(|uid| verify_process(&props, &sha, uid))
        .is_ok();
    let mut result = serde_json::json!({"protocol": 1, "unit": HAND,
        "current_sha256": sha, "service_user": user, "current_release": current.target,
        "load_state": value(&props, "LoadState"), "active_state": value(&props, "ActiveState"),
        "pid": pid(&props)?, "runtime_verified": verified});
    if let Some(id) = transaction {
        let id = valid_id(id)?;
        let path = Path::new(ROOT)
            .join("hand-updates")
            .join(format!("{id}.json"));
        let journal: Journal = serde_json::from_reader(trusted_open(&path, false)?)?;
        if journal.transaction != id || journal.protocol != 1 {
            bail!("journal identity mismatch");
        }
        result["transaction"] = serde_json::json!(id);
        result["phase"] = serde_json::json!(journal.phase);
        result["candidate_sha256"] = serde_json::json!(journal.candidate_sha256);
        result["candidate_release"] = serde_json::json!(journal.release);
        result["previous_release"] = serde_json::json!(journal.original.target);
        result["was_active"] = serde_json::json!(journal.was_active);
        result["machine_id"] = serde_json::json!(journal.machine_id);
    }
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

fn validate_factory_binary(configured: &Path, binary: &Path) -> Result<()> {
    for path in [configured, binary] {
        if !path.is_absolute()
            || !path.starts_with(ROOT)
            || path
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        {
            bail!("factory binary must remain inside the root-owned installation");
        }
    }
    if binary.starts_with(Path::new(ROOT).join("current")) || binary.canonicalize()? != binary {
        bail!("factory runtime must be independently pinned, not the Hand current pointer");
    }
    for ancestor in binary
        .parent()
        .context("factory binary parent")?
        .ancestors()
    {
        trusted_directory(ancestor)?;
    }
    // The configured alias must resolve to a trusted installation executable,
    // even when its active runtime is an older independently pinned release.
    let resolved = configured.canonicalize()?;
    if !resolved.starts_with(ROOT) {
        bail!("factory unit executable resolves outside the root installation");
    }
    for ancestor in resolved
        .parent()
        .context("configured factory binary parent")?
        .ancestors()
    {
        trusted_directory(ancestor)?;
    }
    let _ = release_open(&resolved)?;
    Ok(())
}
fn verify_factory_process(
    pid: u32,
    executable: &Path,
    dev: u64,
    ino: u64,
    sha: &str,
) -> Result<()> {
    let before = process_pin(pid)?;
    if fs::read_link(format!("/proc/{pid}/exe"))? != executable {
        bail!("retained factory executable path changed");
    }
    let mut file = File::open(format!("/proc/{pid}/exe"))?;
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.uid() != 0
        || meta.mode() & 0o022 != 0
        || meta.dev() != dev
        || meta.ino() != ino
        || digest(&mut file)? != sha
    {
        bail!("retained factory executable changed");
    }
    let after = process_pin(pid)?;
    if after.start != before.start || after.uid != before.uid {
        bail!("retained factory PID identity changed");
    }
    Ok(())
}

fn epoch_millis() -> Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis()
        .try_into()?)
}
fn private_json_metadata(path: &Path, uid: u32) -> Result<(serde_json::Value, fs::Metadata)> {
    // Only non-secret status/identity filenames, never environment or account files.
    if !matches!(
        path.file_name().and_then(|v| v.to_str()),
        Some("status.json" | "identity.json")
    ) {
        bail!("invalid readiness filename");
    }
    let parent = path.parent().context("readiness directory missing")?;
    for directory in parent.ancestors() {
        let m = fs::symlink_metadata(directory)?;
        if !m.is_dir() || (m.uid() != uid && m.uid() != 0) || m.mode() & 0o022 != 0 {
            bail!("unsafe readiness directory");
        }
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(NOFOLLOW)
        .open(path)?;
    let m = file.metadata()?;
    if !m.is_file() || m.uid() != uid || m.mode() & 0o077 != 0 || m.nlink() != 1 || m.len() > 65536
    {
        bail!("unsafe readiness file");
    }
    let mut bytes = Vec::new();
    file.take(65537).read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        bail!("readiness file too large");
    }
    let current = fs::symlink_metadata(path)?;
    if current.dev() != m.dev() || current.ino() != m.ino() {
        bail!("readiness file changed during read");
    }
    Ok((serde_json::from_slice(&bytes)?, m))
}
fn private_json(path: &Path, uid: u32) -> Result<serde_json::Value> {
    Ok(private_json_metadata(path, uid)?.0)
}

fn identity_state(args: &[String], user: &str, uid: u32, main: u32) -> Result<(PathBuf, String)> {
    let explicit = args
        .windows(2)
        .find(|a| a[0] == "--state-dir")
        .map(|a| PathBuf::from(&a[1]));
    let passwd = fs::read_to_string("/etc/passwd")?;
    let home = passwd
        .lines()
        .find_map(|line| {
            let p: Vec<_> = line.split(':').collect();
            if p.len() >= 6 && p[0] == user {
                Some(PathBuf::from(p[5]))
            } else {
                None
            }
        })
        .context("service home missing")?;
    let mut directories = Vec::new();
    if let Some(directory) = explicit {
        directories.push(directory);
    } else if args.len() > 2 && args.get(1).map(String::as_str) == Some("hand") {
        directories.push(home.join(".nanocodex2/native-hand"));
    } else {
        let hands = home.join(".nanocodex/hands");
        for entry in fs::read_dir(hands).context("could not locate non-secret existing Hand identity; explicit --state-dir is required for custom HOME")? {
            let entry = entry?; if entry.file_type()?.is_dir() { directories.push(entry.path()); }
        }
    }
    let mut found = Vec::new();
    for directory in directories {
        let identity = private_json(&directory.join("identity.json"), uid)?;
        let id = identity["machine_id"]
            .as_str()
            .context("invalid non-secret Hand identity")?;
        let id = uuid::Uuid::parse_str(id)
            .context("invalid existing machine UUID")?
            .to_string();
        if main != 0 {
            if let Ok(status) = private_json(&directory.join("status.json"), uid) {
                if status["daemon"]["pid"].as_u64() != Some(u64::from(main)) {
                    continue;
                }
                if status["factory"]["status"]
                    .as_str()
                    .is_some_and(|v| v != "unavailable")
                {
                    bail!(
                        "device service has a coupled factory; update requires independent Hand/factory units"
                    );
                }
            } else if args.len() <= 2 || args.get(1).map(String::as_str) == Some("__device-hand") {
                continue;
            }
        }
        found.push((directory, id));
    }
    if found.len() != 1 {
        bail!(
            "existing Hand identity is missing/ambiguous; refusing reenrollment or account migration"
        );
    }
    Ok(found.remove(0))
}
fn readiness(journal: &Journal) -> Result<()> {
    let props = unit(HAND)?;
    let main = pid(&props)?;
    let before = process_pin(main)?;
    verify_process(&props, &journal.candidate_sha256, journal.service_uid)?;
    let path = journal.state_directory.join("status.json");
    let (status, metadata) = private_json_metadata(&path, journal.service_uid)?;
    let modified: u64 = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis()
        .try_into()?;
    validate_readiness(&status, journal, main, modified)?;
    // Connected/screen status must come from this same fresh exact process,
    // not a stale PID/executable or a process that changed while reading it.
    verify_process(&unit(HAND)?, &journal.candidate_sha256, journal.service_uid)?;
    let after = process_pin(main)?;
    if before.start != after.start || before.uid != after.uid || pid(&unit(HAND)?)? != main {
        bail!("candidate PID changed while verifying publisher/screen readiness");
    }
    Ok(())
}
fn validate_readiness(
    status: &serde_json::Value,
    journal: &Journal,
    main: u32,
    modified: u64,
) -> Result<()> {
    if main == 0
        || status["status"] != "connected"
        || status["daemon"]["pid"].as_u64() != Some(u64::from(main))
    {
        bail!("candidate Hand has not freshly published its account catalog");
    }
    if status["screen"]["status"] != "ready" || status["screen"]["transport"] != "webrtc" {
        bail!("candidate native screen publisher is not ready over WebRTC");
    }
    let id = status["machine_id"]
        .as_str()
        .or_else(|| status["machine"]["id"].as_str());
    if id != Some(journal.machine_id.as_str()) {
        bail!("candidate Hand machine identity changed");
    }
    let executable = status["daemon"]["executable"]
        .as_str()
        .context("publisher proof lacks executable")?;
    if Path::new(executable) != journal.release.join("nanocodex2") {
        bail!("publisher proof is not from the frozen candidate");
    }
    if journal.apply_started_millis == 0
        || modified < journal.apply_started_millis
        || status["updated_at_millis"]
            .as_u64()
            .is_some_and(|t| t < journal.apply_started_millis)
    {
        bail!("candidate publisher proof is stale");
    }
    if status["factory"]["status"]
        .as_str()
        .is_some_and(|v| v != "unavailable")
    {
        bail!("candidate unexpectedly started a coupled factory");
    }
    Ok(())
}

fn lifecycle_reaches(from: &str, target: &str) -> Result<bool> {
    // Keep start and stop graphs separate: a shared network/boot prerequisite
    // is not coupling, but a chain of restart/stop propagation is.
    for edges in [
        &["Requires", "Wants", "BindsTo", "Triggers"][..],
        &["ConsistsOf", "BoundBy", "PropagatesStopTo", "RequiredBy"][..],
    ] {
        let mut todo = vec![from.to_owned()];
        let mut seen = std::collections::BTreeSet::new();
        while let Some(name) = todo.pop() {
            if !seen.insert(name.clone()) {
                continue;
            }
            if seen.len() > 256 {
                bail!("systemd dependency graph exceeds safe inspection bound");
            }
            let props = unit(&name)?;
            for edge in edges {
                for next in value(&props, edge).split_whitespace() {
                    if next == target {
                        return Ok(true);
                    }
                    if !seen.contains(next) {
                        todo.push(next.to_owned());
                    }
                }
            }
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn args(command: &str) -> Vec<String> {
        shlex::split(command).unwrap()
    }
    fn journal() -> Journal {
        let original = Pointer {
            target: PathBuf::from("/opt/nanocodex/releases/old"),
            dev: 1,
            ino: 1,
        };
        Journal {
            protocol: 1,
            transaction: "11111111-1111-4111-8111-111111111111".into(),
            candidate_sha256: "a".repeat(64),
            phase: "restarting".into(),
            candidate_complete: true,
            was_active: true,
            start_stopped: false,
            original: original.clone(),
            current: original,
            pending: None,
            pending_path: None,
            old_files: BTreeMap::new(),
            new_files: BTreeMap::new(),
            release: PathBuf::from(
                "/opt/nanocodex/releases/hand-update-11111111-1111-4111-8111-111111111111",
            ),
            service_user: "nanocodex".into(),
            service_uid: 1000,
            state_directory: PathBuf::from("/srv/nanocodex/native-state"),
            machine_id: "22222222-2222-4222-8222-222222222222".into(),
            apply_started_millis: 1000,
            layout: BTreeMap::new(),
            factories: Vec::new(),
            receipts: BTreeMap::new(),
        }
    }
    fn ready(j: &Journal) -> serde_json::Value {
        json!({"status":"connected", "machine_id":j.machine_id, "updated_at_millis":1001,
            "daemon":{"pid":42, "executable":j.release.join("nanocodex2")},
            "screen":{"status":"ready", "transport":"webrtc"}})
    }
    #[test]
    fn accepts_actual_existing_hand_unit_args() {
        validate_hand_args(&args("/opt/nanocodex/current/nanocodex2 hand --workspace /srv/nanocodex/workspace --state-dir /srv/nanocodex/native-state --machine-name linux-paradigm --log-format json --vm-provider linux-paradigm")).unwrap();
        validate_hand_args(&args(
            "/opt/nanocodex/current/nanocodex2 __device-hand --daemon",
        ))
        .unwrap();
        validate_hand_args(&args("/opt/nanocodex/current/nanocodex hand")).unwrap();
        assert!(validate_hand_args(&args("/opt/nanocodex/current/ncl hand")).is_err());
    }
    #[test]
    fn rejects_factory_vm_enrollment_and_ambiguous_args() {
        for suffix in [
            "host",
            "hand --vm /srv/vm",
            "hand --rootfs /srv/rootfs",
            "hand --parent-pipe 1",
            "hand --state-dir /srv/state --state-dir /srv/other",
            "hand --state-dir",
            "hand --state-dir ../state",
            "hand --workspace /srv/../other",
            "hand --log-format unknown",
            "hand --vm-provider --parent-pipe",
        ] {
            assert!(
                validate_hand_args(&args(&format!("{EXE} {suffix}"))).is_err(),
                "accepted {suffix}"
            );
        }
    }
    #[test]
    fn requires_actual_screen_ready_in_addition_to_catalog() {
        let j = journal();
        validate_readiness(&ready(&j), &j, 42, 1001).unwrap();
        for screen in [
            json!(null),
            json!({"status":"unavailable","transport":"webrtc"}),
            json!({"status":"starting","transport":"webrtc"}),
            json!({"status":"ready","transport":"vnc"}),
        ] {
            let mut status = ready(&j);
            status["screen"] = screen;
            assert!(validate_readiness(&status, &j, 42, 1001).is_err());
        }
    }
    #[test]
    fn rejects_stale_wrong_pid_exe_identity_or_coupled_factory_proof() {
        let j = journal();
        for status in [
            {
                let mut v = ready(&j);
                v["status"] = json!("connecting");
                v
            },
            {
                let mut v = ready(&j);
                v["daemon"]["pid"] = json!(41);
                v
            },
            {
                let mut v = ready(&j);
                v["daemon"]["executable"] = json!(EXE);
                v
            },
            {
                let mut v = ready(&j);
                v["machine_id"] = json!("other");
                v
            },
            {
                let mut v = ready(&j);
                v["updated_at_millis"] = json!(999);
                v
            },
            {
                let mut v = ready(&j);
                v["factory"] = json!({"status":"connected"});
                v
            },
        ] {
            assert!(validate_readiness(&status, &j, 42, 1001).is_err());
        }
        assert!(validate_readiness(&ready(&j), &j, 42, 999).is_err());
        assert!(validate_readiness(&ready(&j), &j, 0, 1001).is_err());
        let mut unstarted = journal();
        unstarted.apply_started_millis = 0;
        assert!(validate_readiness(&ready(&j), &unstarted, 42, 1001).is_err());
    }
    #[test]
    fn supports_richer_device_hand_identity_without_native_timestamp() {
        let j = journal();
        let mut status = ready(&j);
        status.as_object_mut().unwrap().remove("machine_id");
        status.as_object_mut().unwrap().remove("updated_at_millis");
        status["machine"] = json!({"id":j.machine_id});
        status["factory"] = json!({"status":"unavailable"});
        validate_readiness(&status, &j, 42, 1001).unwrap();
    }
    #[test]
    fn missing_journal_recovery_requires_no_owned_release_or_pointer_artifacts() {
        let root = tempfile::tempdir().unwrap();
        let id = "11111111-1111-4111-8111-111111111111";
        fs::create_dir(root.path().join("releases")).unwrap();
        require_no_unjournaled_artifacts(root.path(), id).unwrap();
        let release = root
            .path()
            .join("releases")
            .join(format!("hand-update-{id}"));
        fs::create_dir(&release).unwrap();
        assert!(require_no_unjournaled_artifacts(root.path(), id).is_err());
        fs::remove_dir(&release).unwrap();
        let pending = root.path().join(format!(".hand-update-{id}-apply"));
        symlink(root.path().join("nonexistent"), &pending).unwrap();
        assert!(require_no_unjournaled_artifacts(root.path(), id).is_err());
    }
    #[test]
    fn protocol_and_transaction_ids_fail_closed() {
        assert!(
            serde_json::from_value::<Request>(
                json!({"protocol":1,"action":"apply","service":"nanocodex-factory.service"})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<Request>(json!({"protocol":1,"action":"restart"})).is_err()
        );
        assert!(valid_id("00000000-0000-0000-0000-000000000000").is_err());
        assert!(valid_id("11111111-1111-4111-8111-111111111111").is_ok());
        assert!(valid_id("../11111111-1111-4111-8111-111111111111").is_err());
    }
    #[test]
    fn rejects_multiple_or_mismatched_systemd_exec_commands() {
        let mut props = BTreeMap::new();
        props.insert(
            "ExecStart".into(),
            format!("{{ path={EXE} ; argv[]={EXE} hand ; ignore_errors=no }}"),
        );
        assert_eq!(command_argv(&props).unwrap(), args(&format!("{EXE} hand")));
        props.insert("ExecStart".into(), "{ path=/other ; argv[]=/other hand ; ignore_errors=no } { path=/other ; argv[]=/other host ; ignore_errors=no }".into());
        assert!(command_argv(&props).is_err());
        props.insert(
            "ExecStart".into(),
            format!("{{ path={EXE} ; argv[]=/other hand ; ignore_errors=no }}"),
        );
        assert!(command_argv(&props).is_err());
    }
    fn ancillary_props() -> BTreeMap<String, String> {
        [
            ("Id", ANCILLARY),
            ("LoadState", "loaded"),
            ("ActiveState", "inactive"),
            ("SubState", "dead"),
            ("MainPID", "0"),
            ("Type", "oneshot"),
            ("User", "root"),
            ("DynamicUser", "no"),
            ("PrivateUsers", "no"),
            ("Requires", "sysinit.target system.slice docker.service"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .chain([(
            "ExecStart".into(),
            format!("{{ path={ANCILLARY_EXE} ; argv[]={ANCILLARY_EXE} apply ; ignore_errors=no }}"),
        )])
        .collect()
    }
    #[test]
    fn permits_only_the_known_inactive_forwarder_layout() {
        let props = ancillary_props();
        ancillary_layout(&props).unwrap();
        for user in ["", "root", "0"] {
            let mut p = props.clone();
            p.insert("User".into(), user.into());
            ancillary_layout(&p).unwrap();
        }
        for (key, invalid) in [
            ("Id", "nanocodex-other.service"),
            ("LoadState", "not-found"),
            ("ActiveState", "active"),
            ("SubState", "running"),
            ("MainPID", "123"),
            ("MainPID", "invalid"),
            ("Type", "forking"),
            ("User", "nanocodex"),
            ("DynamicUser", "yes"),
            ("PrivateUsers", "yes"),
            ("ExecStartPre", "/bin/true"),
            ("ExecStartPost", "/bin/true"),
            ("ExecStop", "/bin/true"),
            ("ExecStopPost", "/bin/true"),
            ("ExecReload", "/bin/true"),
            ("ExecCondition", "/bin/true"),
            ("RootDirectory", "/other"),
            ("RootImage", "/other"),
            ("BindsTo", "docker.service"),
            ("PartOf", "docker.service"),
            (
                "FragmentPath",
                "/opt/nanocodex/releases/old/forwarder.service",
            ),
            ("Requires", HAND),
        ] {
            let mut p = props.clone();
            p.insert(key.into(), invalid.into());
            // Direct dependency rejection is performed by verify_ancillary.
            assert!(
                ancillary_layout(&p).is_err() || dependent(&p, HAND),
                "accepted {key}={invalid}"
            );
        }
    }
    #[test]
    fn rejects_forwarder_runtime_paths_alias_argv_and_extra_commands() {
        for command in [
            format!("{EXE} hand"),
            "/opt/nanocodex/releases/old/nanocodex2 host".into(),
            format!("{ANCILLARY_EXE} host"),
            format!("{ANCILLARY_EXE} apply extra"),
            format!("{ANCILLARY_EXE} apply --state-dir /opt/nanocodex/current"),
            "/usr/local/bin/nanocodex2 apply".into(),
        ] {
            let mut p = ancillary_props();
            let executable = command.split_whitespace().next().unwrap();
            p.insert(
                "ExecStart".into(),
                format!("{{ path={executable} ; argv[]={command} ; ignore_errors=no }}"),
            );
            assert!(ancillary_layout(&p).is_err(), "accepted {command}");
        }
        let mut p = ancillary_props();
        p.get_mut("ExecStart")
            .unwrap()
            .push_str(" { path=/bin/true ; argv[]=/bin/true ; ignore_errors=no }");
        assert!(ancillary_layout(&p).is_err());
        p.insert(
            "ExecStart".into(),
            format!("{{ path={ANCILLARY_EXE} ; argv[]=/other apply ; ignore_errors=no }}"),
        );
        assert!(ancillary_layout(&p).is_err());
    }
    #[test]
    fn discovers_unknown_runtime_names_commands_hooks_and_resolved_aliases() {
        let mut p = BTreeMap::new();
        assert!(runtime_unit("nanocodex-unknown.service", &p));
        assert!(!runtime_unit("unrelated.service", &p));
        for command in [
            EXE,
            "/opt/nanocodex/releases/old/nanocodex2",
            "/usr/local/bin/nanocodex2",
            "/usr/local/bin/nanocodex",
        ] {
            for key in [
                "ExecStart",
                "ExecStartPre",
                "ExecStartPost",
                "ExecStop",
                "ExecStopPost",
                "ExecReload",
                "ExecCondition",
            ] {
                p.clear();
                p.insert(key.into(), format!("{{ path=/bin/true ; argv[]=/bin/true ; ignore_errors=no }} {{ path={command} ; argv[]={command} host ; ignore_errors=no }}"));
                assert!(
                    runtime_unit("unrelated.service", &p),
                    "missed {key} {command}"
                );
            }
        }
        for resolved in [EXE, "/opt/nanocodex/releases/old/nanocodex2"] {
            assert!(runtime_executable(
                Path::new("/usr/local/bin/alias"),
                Some(Path::new(resolved))
            ));
        }
        assert!(!runtime_executable(
            Path::new(ANCILLARY_EXE),
            Some(Path::new(ANCILLARY_EXE))
        ));
    }
    #[test]
    fn rejects_ancillary_symlinks_and_untrusted_executables() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("binary");
        fs::write(&binary, b"untrusted").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(ancillary_binary(&binary).is_err());
        let alias = dir.path().join("alias");
        symlink(&binary, &alias).unwrap();
        assert!(ancillary_binary(&alias).is_err());
        let release_alias = dir.path().join("release-alias");
        symlink(EXE, &release_alias).unwrap();
        assert!(ancillary_binary(&release_alias).is_err());
    }
    #[test]
    fn ancillary_service_and_timer_direct_coupling_remains_rejected() {
        let props = ancillary_props();
        for name in [ANCILLARY, "nanocodex-win11-webrtc.timer"] {
            assert!(!dependent(&props, HAND));
            for key in [
                "Requires",
                "Wants",
                "BindsTo",
                "PartOf",
                "ConsistsOf",
                "RequiredBy",
                "BoundBy",
                "PropagatesStopTo",
                "StopPropagatedFrom",
                "Triggers",
                "TriggeredBy",
            ] {
                let mut p = props.clone();
                p.insert(key.into(), HAND.into());
                assert!(dependent(&p, HAND), "missed {key}");
                p.insert(key.into(), name.into());
                assert!(dependent(&p, name), "missed reverse {key}");
            }
        }
    }
    #[test]
    fn elf_header_checks_arch_and_format() {
        let mut data = [0u8; 20];
        data[..4].copy_from_slice(b"\x7fELF");
        data[4] = 2;
        data[5] = 1;
        data[16..18].copy_from_slice(&3u16.to_le_bytes());
        let arch: u16 = if cfg!(target_arch = "x86_64") {
            62
        } else {
            183
        };
        data[18..20].copy_from_slice(&arch.to_le_bytes());
        let mut file = tempfile::tempfile().unwrap();
        std::io::Write::write_all(&mut file, &data).unwrap();
        file.rewind().unwrap();
        validate_elf(&mut file).unwrap();
        data[4] = 1;
        file.rewind().unwrap();
        std::io::Write::write_all(&mut file, &data).unwrap();
        file.rewind().unwrap();
        assert!(validate_elf(&mut file).is_err());
    }
    #[test]
    fn journal_is_private_and_trusted_open_rejects_links() {
        if !nix::unistd::geteuid().is_root() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.json");
        save(&path, &journal()).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        let hard = dir.path().join("hard.json");
        fs::hard_link(&path, &hard).unwrap();
        assert!(trusted_open(&path, false).is_err());
        fs::remove_file(&hard).unwrap();
        let link = dir.path().join("link.json");
        symlink(&path, &link).unwrap();
        assert!(trusted_open(&link, false).is_err());
    }
    #[test]
    fn accepts_prior_companions_hardlinked_only_within_trusted_releases() {
        if !nix::unistd::geteuid().is_root() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let releases = dir.path().join("releases");
        fs::create_dir(&releases).unwrap();
        let old = releases.join("old");
        let retained = releases.join("retained");
        fs::create_dir(&old).unwrap();
        fs::create_dir(&retained).unwrap();
        let path = old.join("companion");
        fs::write(&path, b"prior companion").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        fs::hard_link(&path, retained.join("companion")).unwrap();
        release_metadata_under(&releases, &path, &fs::metadata(&path).unwrap()).unwrap();
        fs::hard_link(&path, dir.path().join("foreign")).unwrap();
        assert!(release_metadata_under(&releases, &path, &fs::metadata(&path).unwrap()).is_err());
    }
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "read-only installed service diagnostic; never mutates units/services or account data"]
    fn installed_service_layout_readonly() {
        let props = unit(HAND).unwrap();
        validate_hand_args(&command_argv(&props).unwrap()).unwrap();
        unit_layout(&props).unwrap();
        let retained = factories(&props).unwrap();
        for factory in retained {
            println!(
                "retained unit={} pid={} pinned_processes={}",
                factory.unit,
                factory.main,
                factory.pins.len()
            );
        }
    }
    #[test]
    fn rejects_foreign_hardlinked_release_companion() {
        if !nix::unistd::geteuid().is_root() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("companion");
        fs::write(&path, b"safe bytes").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        release_open(&path).unwrap();
        fs::hard_link(&path, dir.path().join("other")).unwrap();
        assert!(release_open(&path).is_err());
    }
}
