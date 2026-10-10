//! Linux capture selection never borrows another user's login session. Environment
//! values are hints, not proof of a compositor: validate ownership and connect to
//! its Unix socket before selecting it. No process-global environment is changed.
use nanocodex_managed::ManagedError;
use std::{
    ffi::{OsStr, OsString},
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Component, Path, PathBuf},
};

type Result<T> = std::result::Result<T, ManagedError>;
fn error(message: impl std::fmt::Display) -> ManagedError {
    ManagedError::Configuration(format!("Linux screen session: {message}"))
}

#[derive(Clone, Debug)]
pub(crate) struct WaylandSession {
    runtime: PathBuf,
    display: OsString,
    uid: u32,
}
pub(crate) enum Selection {
    Wayland(WaylandSession),
    PrivateDesktop,
}

pub(crate) fn select() -> Result<Selection> {
    let uid = nix::unistd::Uid::effective().as_raw();
    select_with(
        std::env::var_os("NANOCODEX_SCREEN_BACKEND").as_deref(),
        std::env::var_os("XDG_RUNTIME_DIR").as_deref(),
        std::env::var_os("WAYLAND_DISPLAY").as_deref(),
        &PathBuf::from(format!("/run/user/{uid}")),
        uid,
    )
}

fn select_with(
    backend: Option<&OsStr>,
    runtime: Option<&OsStr>,
    display: Option<&OsStr>,
    standard_runtime: &Path,
    uid: u32,
) -> Result<Selection> {
    let explicit = match backend {
        None => false,
        Some(value) if value == "default" || value == "auto" => false,
        Some(value) if value == "wayland" => true,
        // Preserve the private Xvfb override even in a valid Wayland session.
        Some(value) if value == "x11" || value == "xvfb" => {
            return Ok(Selection::PrivateDesktop);
        }
        Some(_) => {
            return Err(error(
                "backend must be default, auto, wayland, x11, or xvfb",
            ));
        }
    };
    let session = discover(runtime, display, standard_runtime, uid)?;
    match session {
        Some(session) => Ok(Selection::Wayland(session)),
        None if explicit => Err(error(
            "explicit Wayland backend requires a live same-uid compositor socket in a private runtime directory",
        )),
        None => Ok(Selection::PrivateDesktop),
    }
}

fn discover(
    runtime: Option<&OsStr>,
    display: Option<&OsStr>,
    standard_runtime: &Path,
    uid: u32,
) -> Result<Option<WaylandSession>> {
    let runtime = runtime.filter(|value| !value.is_empty()).map(PathBuf::from);
    let display = display.filter(|value| !value.is_empty());
    // Honour an unambiguous valid hint first. An invalid/stale inherited hint
    // cannot route capture to a foreign socket or force a Wayland downgrade.
    if let Some(display) = display {
        let directory = runtime.as_deref().unwrap_or(standard_runtime);
        if let Ok(session) = WaylandSession::validated(directory, display, uid) {
            return Ok(Some(session));
        }
    }
    // Scan only a validated inherited runtime and our own standard runtime.
    // In particular, do not enumerate /run/user or read /proc/*/environ.
    let mut sessions = Vec::new();
    let mut directories = Vec::new();
    if let Some(runtime) = runtime {
        directories.push(runtime);
    }
    if !directories.iter().any(|path| path == standard_runtime) {
        directories.push(standard_runtime.to_owned());
    }
    for directory in directories {
        if validate_runtime(&directory, uid).is_err() {
            continue;
        }
        let entries = std::fs::read_dir(&directory).map_err(error)?;
        for entry in entries {
            let entry = entry.map_err(error)?;
            let name = entry.file_name();
            // Lock files, nested paths and unrelated private IPC are not displays.
            let Some(suffix) = name.to_str().and_then(|name| name.strip_prefix("wayland-")) else {
                continue;
            };
            if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
                continue;
            }
            if let Ok(session) = WaylandSession::validated(&directory, &name, uid) {
                sessions.push(session);
            }
        }
    }
    match sessions.len() {
        0 => Ok(None),
        1 => Ok(sessions.pop()),
        _ => Err(error(
            "multiple live same-uid Wayland displays; set XDG_RUNTIME_DIR and WAYLAND_DISPLAY explicitly",
        )),
    }
}

fn validate_runtime(runtime: &Path, uid: u32) -> Result<()> {
    if !runtime.is_absolute() {
        return Err(error("runtime directory must be absolute"));
    }
    // Reject symlink traversal as well as a symlink at the directory itself.
    let mut walked = PathBuf::new();
    for component in runtime.components() {
        match component {
            Component::RootDir | Component::Normal(_) => walked.push(component.as_os_str()),
            _ => {
                return Err(error(
                    "runtime directory must not contain relative components",
                ));
            }
        }
        let metadata = std::fs::symlink_metadata(&walked).map_err(error)?;
        if metadata.file_type().is_symlink() {
            return Err(error("runtime directory must not traverse symlinks"));
        }
    }
    let metadata = std::fs::symlink_metadata(runtime).map_err(error)?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        return Err(error(
            "runtime directory must be a private same-uid directory",
        ));
    }
    Ok(())
}

impl WaylandSession {
    fn validated(runtime: &Path, display: &OsStr, uid: u32) -> Result<Self> {
        let display_path = Path::new(display);
        // libwayland permits absolute display paths, but only direct children of
        // the validated runtime are accepted here. Never accept ../ or symlinks.
        let name = if display_path.is_absolute() {
            if display_path.parent() != Some(runtime) {
                return Err(error(
                    "display socket must be inside its private runtime directory",
                ));
            }
            display_path
                .file_name()
                .ok_or_else(|| error("empty display"))?
        } else {
            let mut components = display_path.components();
            match (components.next(), components.next()) {
                (Some(Component::Normal(name)), None) => name,
                _ => return Err(error("display must name a single socket")),
            }
        };
        let session = Self {
            runtime: runtime.to_owned(),
            display: name.to_owned(),
            uid,
        };
        session.validate()?;
        Ok(session)
    }

    fn validate(&self) -> Result<()> {
        use nix::sys::socket::{
            AddressFamily, SockFlag, SockType, UnixAddr, connect, getsockopt, socket,
            sockopt::PeerCredentials,
        };
        use std::os::fd::AsRawFd;
        validate_runtime(&self.runtime, self.uid)?;
        let path = self.runtime.join(&self.display);
        let before = std::fs::symlink_metadata(&path).map_err(error)?;
        if !before.file_type().is_socket() || before.uid() != self.uid {
            return Err(error("display must be a same-uid non-symlink Unix socket"));
        }
        // Nonblocking connect bounds discovery even for a full listener backlog;
        // a stale pathname is insufficient evidence of a running compositor.
        let socket = socket(
            AddressFamily::Unix,
            SockType::Stream,
            SockFlag::SOCK_CLOEXEC | SockFlag::SOCK_NONBLOCK,
            None,
        )
        .map_err(error)?;
        let address = UnixAddr::new(&path).map_err(error)?;
        connect(socket.as_raw_fd(), &address).map_err(error)?;
        let peer = getsockopt(&socket, PeerCredentials).map_err(error)?;
        if peer.uid() != self.uid {
            return Err(error("Wayland compositor peer has a different uid"));
        }
        let after = std::fs::symlink_metadata(&path).map_err(error)?;
        if before.dev() != after.dev() || before.ino() != after.ino() {
            return Err(error("display socket changed during validation"));
        }
        Ok(())
    }

    // Legacy opt-in text helpers inherit their child environment. Require it
    // to name this captured session; never send text to a different compositor.
    // Default Waymote IME delivery already uses the configured capture child.
    pub(crate) fn validate_inherited_text_session(&self) -> Result<()> {
        self.validate()?;
        self.validate_text_environment(
            std::env::var_os("XDG_RUNTIME_DIR").as_deref(),
            std::env::var_os("WAYLAND_DISPLAY").as_deref(),
        )
    }

    fn validate_text_environment(
        &self,
        runtime: Option<&OsStr>,
        display: Option<&OsStr>,
    ) -> Result<()> {
        let runtime = runtime
            .filter(|value| !value.is_empty())
            .ok_or_else(|| error("opt-in text helpers require the captured XDG_RUNTIME_DIR"))?;
        let display = display
            .filter(|value| !value.is_empty())
            .ok_or_else(|| error("opt-in text helpers require the captured WAYLAND_DISPLAY"))?;
        let inherited = Self::validated(Path::new(runtime), display, self.uid)?;
        if inherited.runtime != self.runtime || inherited.display != self.display {
            return Err(error(
                "opt-in text helper environment differs from the captured Wayland session",
            ));
        }
        Ok(())
    }

    pub(crate) fn configure(&self, command: &mut tokio::process::Command) -> Result<()> {
        self.validate()?;
        command
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env("WAYLAND_DISPLAY", &self.display);
        Ok(())
    }
}

// Output ownership belongs to the user's compositor, not the Hand lifetime.
// Never remove this output on shutdown or reconfigure any pre-existing output.
const OWNED_OUTPUT: &str = "NANOCODEX-HEADLESS-1";
const HYPRCTL_LIMIT: usize = 64 * 1024;

fn hyprland_instance(json: &str, display: &str) -> Result<Option<String>> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(error)?;
    let instances = value
        .as_array()
        .ok_or_else(|| error("invalid Hyprland instances"))?;
    let mut selected = None;
    for instance in instances {
        let socket = instance["wl_socket"]
            .as_str()
            .ok_or_else(|| error("invalid Hyprland socket"))?;
        if socket != display {
            continue;
        }
        let signature = instance["instance"]
            .as_str()
            .ok_or_else(|| error("missing Hyprland signature"))?;
        if signature.is_empty()
            || signature.len() > 128
            || !signature
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(error("unsafe Hyprland instance signature"));
        }
        if selected.replace(signature.to_owned()).is_some() {
            return Err(error("ambiguous Hyprland instance for captured display"));
        }
    }
    Ok(selected)
}

fn hyprland_monitors(json: &str) -> Result<Vec<serde_json::Value>> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(error)?;
    let monitors = value
        .as_array()
        .ok_or_else(|| error("invalid Hyprland monitors"))?;
    if monitors
        .iter()
        .any(|m| m["name"].as_str().is_none_or(str::is_empty))
    {
        return Err(error("invalid Hyprland monitor name"));
    }
    Ok(monitors.clone())
}

// The injected runner uses owned arguments so tests never touch a compositor.
async fn prepare_hyprland_with<F, Fut>(display: &str, mut run: F) -> Result<()>
where
    F: FnMut(Option<String>, Vec<String>) -> Fut,
    Fut: std::future::Future<Output = Result<String>>,
{
    let args = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect();
    let instances = run(None, args(&["instances", "-j"])).await?;
    let Some(signature) = hyprland_instance(&instances, display)? else {
        return Ok(());
    };
    let monitors = run(Some(signature.clone()), args(&["monitors", "-j"])).await?;
    if !hyprland_monitors(&monitors)?.is_empty() {
        return Ok(());
    }
    let created = run(
        Some(signature.clone()),
        args(&["output", "create", "headless", OWNED_OUTPUT]),
    )
    .await?;
    if created.trim() != "ok" {
        return Err(error("Hyprland headless output creation rejected"));
    }
    let eval = format!(
        "hl.monitor({{ output=\"{OWNED_OUTPUT}\", mode=\"3840x2160@60\", position=\"0x0\", scale=2 }})"
    );
    // Only a definite compositor rejection permits the legacy syntax fallback;
    // transport/timeouts are uncertain mutations and must not be retried.
    let configured = run(Some(signature.clone()), args(&["eval", &eval])).await?;
    if configured.trim() != "ok" {
        let legacy = format!("{OWNED_OUTPUT},3840x2160@60,0x0,2");
        let reply = run(
            Some(signature.clone()),
            args(&["keyword", "monitor", &legacy]),
        )
        .await?;
        if reply.trim() != "ok" {
            return Err(error("Hyprland headless output configuration rejected"));
        }
    }
    let verified = run(Some(signature), args(&["monitors", "-j"])).await?;
    if !hyprland_monitors(&verified)?.iter().any(|m| {
        m["name"] == OWNED_OUTPUT
            && m["width"] == 3840
            && m["height"] == 2160
            && m["scale"].as_f64() == Some(2.0)
            && m["refreshRate"]
                .as_f64()
                .is_some_and(|rate| (rate - 60.0).abs() < 1.0)
    }) {
        return Err(error(
            "Hyprland did not expose the configured Nanocodex output",
        ));
    }
    Ok(())
}

impl WaylandSession {
    pub(crate) async fn prepare_output(&self) -> Result<()> {
        self.validate()?;
        if !Path::new("/usr/bin/hyprctl").is_file() {
            return Ok(());
        }
        let display = self
            .display
            .to_str()
            .ok_or_else(|| error("invalid display name"))?;
        prepare_hyprland_with(display, |signature, args| self.hyprctl(signature, args)).await
    }

    async fn hyprctl(&self, signature: Option<String>, args: Vec<String>) -> Result<String> {
        use std::{process::Stdio, time::Duration};
        use tokio::io::AsyncReadExt;
        let mut command = tokio::process::Command::new("/usr/bin/hyprctl");
        self.configure(&mut command)?;
        command.env_remove("HYPRLAND_INSTANCE_SIGNATURE");
        if let Some(signature) = signature {
            command.env("HYPRLAND_INSTANCE_SIGNATURE", signature);
        }
        let eval = args.first().is_some_and(|arg| arg == "eval");
        let mut child = command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(error)?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| error("hyprctl stdout missing"))?
            .take((HYPRCTL_LIMIT + 1) as u64);
        let mut bytes = Vec::new();
        let result = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::try_join!(stdout.read_to_end(&mut bytes), child.wait())
        })
        .await;
        let (_, status) = match result {
            Ok(result) => result.map_err(error)?,
            Err(_) => {
                let _ = child.start_kill();
                let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
                return Err(error("hyprctl timed out for captured Wayland session"));
            }
        };
        if bytes.len() > HYPRCTL_LIMIT {
            return Err(error("hyprctl output exceeded limit"));
        }
        if !status.success() {
            if eval {
                return Ok("eval rejected".into());
            }
            return Err(error("hyprctl failed for captured Wayland session"));
        }
        String::from_utf8(bytes).map_err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::{
        fs::{PermissionsExt, symlink},
        net::UnixListener,
    };

    struct Fixture {
        directory: tempfile::TempDir,
        uid: u32,
    }
    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
            Self {
                directory,
                uid: nix::unistd::Uid::effective().as_raw(),
            }
        }
        fn socket(&self, name: &str) -> UnixListener {
            UnixListener::bind(self.directory.path().join(name)).unwrap()
        }
        fn select(
            &self,
            backend: Option<&str>,
            runtime: Option<&OsStr>,
            display: Option<&str>,
        ) -> Result<Selection> {
            select_with(
                backend.map(OsStr::new),
                runtime,
                display.map(OsStr::new),
                self.directory.path(),
                self.uid,
            )
        }
    }

    // Session discovery cannot be exercised against a real login in CI without
    // desktop access. Real private Unix listeners exercise liveness and peer uid
    // checks while never connecting to or injecting into the user's compositor.
    #[test]
    fn default_and_explicit_backend_compatibility() {
        let f = Fixture::new();
        assert!(matches!(
            f.select(None, None, None).unwrap(),
            Selection::PrivateDesktop
        ));
        assert!(f.select(Some("wayland"), None, None).is_err());
        let _socket = f.socket("wayland-0");
        for backend in [None, Some("default"), Some("auto"), Some("wayland")] {
            assert!(matches!(
                f.select(backend, None, None).unwrap(),
                Selection::Wayland(_)
            ));
        }
        for backend in ["x11", "xvfb"] {
            assert!(matches!(
                f.select(Some(backend), Some(OsStr::new("/invalid")), Some("stale"))
                    .unwrap(),
                Selection::PrivateDesktop
            ));
        }
        for backend in ["", "typo"] {
            assert!(f.select(Some(backend), None, None).is_err());
        }
    }

    #[test]
    fn empty_stale_invalid_and_foreign_hints_never_become_sessions() {
        let f = Fixture::new();
        let stale = f.socket("wayland-0");
        drop(stale);
        for display in [
            None,
            Some(""),
            Some("wayland-0"),
            Some("../wayland-0"),
            Some("/foreign/wayland-0"),
        ] {
            assert!(matches!(
                f.select(None, Some(OsStr::new("")), display).unwrap(),
                Selection::PrivateDesktop
            ));
            assert!(
                f.select(Some("wayland"), Some(OsStr::new("")), display)
                    .is_err()
            );
        }
        std::fs::write(f.directory.path().join("wayland-9"), b"not a socket").unwrap();
        assert!(
            WaylandSession::validated(f.directory.path(), OsStr::new("wayland-9"), f.uid).is_err()
        );
        let _live = f.socket("wayland-1");
        assert!(
            WaylandSession::validated(
                f.directory.path(),
                f.directory.path().join("wayland-1").as_os_str(),
                f.uid
            )
            .is_ok()
        );
        assert!(
            WaylandSession::validated(
                f.directory.path(),
                OsStr::new("wayland-1"),
                f.uid.wrapping_add(1)
            )
            .is_err()
        );
        // A bad inherited runtime cannot block discovery of our own fallback.
        assert!(matches!(
            f.select(None, Some(OsStr::new("/foreign")), Some("stale"))
                .unwrap(),
            Selection::Wayland(_)
        ));
        std::fs::set_permissions(f.directory.path(), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        assert!(matches!(
            f.select(None, None, Some("wayland-1")).unwrap(),
            Selection::PrivateDesktop
        ));
        assert!(f.select(Some("wayland"), None, Some("wayland-1")).is_err());
    }

    #[test]
    fn symlink_runtime_socket_and_ambiguous_sessions_are_rejected() {
        let f = Fixture::new();
        let _one = f.socket("wayland-0");
        let _two = f.socket("wayland-1");
        assert!(f.select(None, None, None).is_err());
        assert!(f.select(Some("wayland"), None, Some("invalid")).is_err());
        let selected = f.select(None, None, Some("wayland-1")).unwrap();
        let Selection::Wayland(session) = selected else {
            panic!("expected session")
        };
        assert_eq!(session.display, "wayland-1");
        let outer = Fixture::new();
        let link = outer.directory.path().join("runtime");
        symlink(f.directory.path(), &link).unwrap();
        assert!(WaylandSession::validated(&link, OsStr::new("wayland-0"), f.uid).is_err());
        symlink(
            f.directory.path().join("wayland-0"),
            outer.directory.path().join("wayland-0"),
        )
        .unwrap();
        assert!(
            outer
                .select(Some("wayland"), None, Some("wayland-0"))
                .is_err()
        );
    }

    #[test]
    fn child_environment_is_captured_and_revalidated_without_global_mutation() {
        let f = Fixture::new();
        let live = f.socket("wayland-0");
        let Selection::Wayland(session) = f.select(None, None, None).unwrap() else {
            panic!("expected session")
        };
        let _other = f.socket("wayland-1");
        assert!(session.validate_text_environment(None, None).is_err());
        assert!(
            session
                .validate_text_environment(Some(OsStr::new("")), Some(OsStr::new("")))
                .is_err()
        );
        assert!(
            session
                .validate_text_environment(
                    Some(f.directory.path().as_os_str()),
                    Some(OsStr::new("wayland-0"))
                )
                .is_ok()
        );
        assert!(
            session
                .validate_text_environment(
                    Some(f.directory.path().as_os_str()),
                    Some(OsStr::new("wayland-1"))
                )
                .is_err()
        );
        let before_runtime = std::env::var_os("XDG_RUNTIME_DIR");
        let before_display = std::env::var_os("WAYLAND_DISPLAY");
        let mut child = tokio::process::Command::new("/bin/sh");
        child.args([
            "-c",
            "printf '%s\n%s\n' \"$XDG_RUNTIME_DIR\" \"$WAYLAND_DISPLAY\"",
        ]);
        session.configure(&mut child).unwrap();
        let environment: Vec<_> = child.as_std().get_envs().collect();
        assert!(environment.contains(&(
            OsStr::new("XDG_RUNTIME_DIR"),
            Some(f.directory.path().as_os_str())
        )));
        assert!(
            environment.contains(&(OsStr::new("WAYLAND_DISPLAY"), Some(OsStr::new("wayland-0"))))
        );
        let output = child.as_std_mut().output().unwrap();
        assert!(output.status.success());
        assert_eq!(
            output.stdout,
            format!("{}\nwayland-0\n", f.directory.path().display()).into_bytes()
        );
        assert_eq!(std::env::var_os("XDG_RUNTIME_DIR"), before_runtime);
        assert_eq!(std::env::var_os("WAYLAND_DISPLAY"), before_display);
        drop(live);
        assert!(
            session.configure(&mut child).is_err(),
            "recovery must not trust stale captured socket"
        );
    }
}

#[cfg(test)]
mod hyprland_tests {
    use super::*;
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    const INSTANCE: &str = r#"[{"instance":"safe_123-ab","wl_socket":"wayland-1"}]"#;
    const VERIFIED: &str = r#"[{"name":"NANOCODEX-HEADLESS-1","width":3840,"height":2160,"scale":2.0,"refreshRate":60.0}]"#;

    async fn fixture(
        replies: Vec<Result<String>>,
    ) -> (Result<()>, Vec<(Option<String>, Vec<String>)>) {
        let replies = Arc::new(Mutex::new(VecDeque::from(replies)));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let trace = calls.clone();
        let result = prepare_hyprland_with("wayland-1", move |signature, args| {
            calls.lock().unwrap().push((signature, args));
            let reply = replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected command");
            std::future::ready(reply)
        })
        .await;
        let calls = trace.lock().unwrap().clone();
        (result, calls)
    }
    fn replies(values: &[&str]) -> Vec<Result<String>> {
        values.iter().map(|s| Ok((*s).into())).collect()
    }

    #[test]
    fn exact_same_session_signature_is_bounded_and_unambiguous() {
        assert_eq!(
            hyprland_instance(INSTANCE, "wayland-1").unwrap().as_deref(),
            Some("safe_123-ab")
        );
        for display in ["wayland-0", "wayland-10", "/foreign/wayland-1"] {
            assert!(hyprland_instance(INSTANCE, display).unwrap().is_none());
        }
        for signature in [
            "".to_owned(),
            "../foreign".into(),
            "a;b".into(),
            "x".repeat(129),
        ] {
            let data =
                serde_json::json!([{"instance": signature, "wl_socket":"wayland-1"}]).to_string();
            assert!(hyprland_instance(&data, "wayland-1").is_err());
        }
        let duplicate = serde_json::json!([
            {"instance":"a", "wl_socket":"wayland-1"},
            {"instance":"b", "wl_socket":"wayland-1"}
        ])
        .to_string();
        assert!(hyprland_instance(&duplicate, "wayland-1").is_err());
        assert!(
            OWNED_OUTPUT.len() < 64
                && OWNED_OUTPUT
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        );
    }

    #[tokio::test]
    async fn absent_or_foreign_hyprland_is_noop() {
        for instances in ["[]", r#"[{"instance":"foreign","wl_socket":"wayland-0"}]"#] {
            let (result, calls) = fixture(replies(&[instances])).await;
            assert!(result.is_ok());
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0], (None, vec!["instances".into(), "-j".into()]));
        }
    }

    #[tokio::test]
    async fn all_existing_outputs_remain_untouched_on_start_and_recovery() {
        for monitors in [
            r#"[{"name":"DP-1"}]"#,
            r#"[{"name":"HEADLESS-1"}]"#,
            VERIFIED,
        ] {
            for _ in 0..2 {
                let (result, calls) = fixture(replies(&[INSTANCE, monitors])).await;
                assert!(result.is_ok());
                assert_eq!(calls.len(), 2);
                assert_eq!(calls[1].0.as_deref(), Some("safe_123-ab"));
                assert_eq!(calls[1].1, ["monitors", "-j"]);
            }
        }
    }

    #[tokio::test]
    async fn zero_output_creates_configures_and_verifies_only_owned_name() {
        let (result, calls) = fixture(replies(&[INSTANCE, "[]", "ok", "ok", VERIFIED])).await;
        assert!(result.is_ok());
        assert_eq!(calls.len(), 5);
        assert_eq!(calls[2].1, ["output", "create", "headless", OWNED_OUTPUT]);
        assert_eq!(
            calls[3].1,
            [
                "eval",
                &format!(
                    "hl.monitor({{ output=\"{OWNED_OUTPUT}\", mode=\"3840x2160@60\", position=\"0x0\", scale=2 }})"
                )
            ]
        );
        assert_eq!(calls[4].1, ["monitors", "-j"]);
        assert!(
            calls
                .iter()
                .skip(1)
                .all(|c| c.0.as_deref() == Some("safe_123-ab"))
        );
    }

    #[tokio::test]
    async fn definite_eval_rejection_uses_bounded_legacy_fallback() {
        let (result, calls) = fixture(replies(&[
            INSTANCE,
            "[]",
            "ok",
            "unknown request",
            "ok",
            VERIFIED,
        ]))
        .await;
        assert!(result.is_ok());
        assert_eq!(calls.len(), 6);
        assert_eq!(
            calls[4].1,
            [
                "keyword",
                "monitor",
                &format!("{OWNED_OUTPUT},3840x2160@60,0x0,2")
            ]
        );
    }

    #[tokio::test]
    async fn malformed_rejected_and_unverified_results_fail_closed() {
        for values in [
            vec!["not json"],
            vec!["{}"],
            vec![INSTANCE, "{}"],
            vec![INSTANCE, "[{}]"],
            vec![INSTANCE, "[]", "not ok"],
            vec![INSTANCE, "[]", "ok", "ok", "[]"],
            vec![
                INSTANCE,
                "[]",
                "ok",
                "ok",
                r#"[{"name":"DP-1","width":3840,"height":2160,"scale":2,"refreshRate":60}]"#,
            ],
            vec![
                INSTANCE,
                "[]",
                "ok",
                "ok",
                r#"[{"name":"NANOCODEX-HEADLESS-1","width":1920,"height":1080,"scale":1,"refreshRate":60}]"#,
            ],
            vec![INSTANCE, "[]", "ok", "rejected", "rejected"],
        ] {
            assert!(fixture(replies(&values)).await.0.is_err());
        }
    }

    #[tokio::test]
    async fn timeout_or_transport_failure_never_retries_mutations() {
        for stage in 0..5 {
            let mut values = replies(&[INSTANCE, "[]", "ok", "ok", VERIFIED][..stage]);
            values.push(Err(error("hyprctl timed out")));
            let (result, calls) = fixture(values).await;
            assert!(result.is_err());
            assert_eq!(calls.len(), stage + 1);
        }
    }
}
