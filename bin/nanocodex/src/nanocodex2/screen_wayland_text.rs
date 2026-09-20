//! Opt-in text backends. Selection happens before delivery; errors never retry text.
use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use std::{
    ffi::OsString, io, os::unix::fs::PermissionsExt, path::PathBuf, process::Stdio, time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};

struct Config {
    x11: bool,
    display: bool,
    wtype: bool,
    path: OsString,
}
impl Config {
    fn environment() -> Self {
        Self {
            x11: std::env::var("NANOCODEX_WAYLAND_TEXT_X11").as_deref() == Ok("1"),
            display: std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty()),
            wtype: std::env::var("NANOCODEX_WAYLAND_TEXT_WTYPE").as_deref() == Ok("1"),
            path: std::env::var_os("PATH").unwrap_or_default(),
        }
    }
    fn executable(&self, name: &str) -> io::Result<Option<PathBuf>> {
        let mut denied = None;
        for directory in std::env::split_paths(&self.path) {
            let path = directory.join(name);
            match std::fs::metadata(&path) {
                Ok(meta) if meta.is_file() && meta.permissions().mode() & 0o111 != 0 => {
                    return Ok(Some(path));
                }
                Ok(_) => {
                    denied = Some(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "text helper is not executable",
                    ))
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => denied = Some(e),
            }
        }
        denied.map_or(Ok(None), Err)
    }
}

pub(super) async fn type_text(text: &str) -> io::Result<bool> {
    type_with(&Config::environment(), text).await
}
async fn type_with(config: &Config, text: &str) -> io::Result<bool> {
    tokio::time::timeout(Duration::from_secs(5), async {
        if config.x11
            && config.display
            && let Some(executable) = config.executable("xdotool")?
            && focused(&executable).await?
        {
            let result = run(
                &executable,
                &["type", "--clearmodifiers", "--delay", "1", "--file", "-"],
                text.as_bytes(),
                Duration::from_secs(5),
            )
            .await?;
            successful(result)?;
            return Ok(true);
        }
        if config.wtype {
            let executable = config
                .executable("wtype")?
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "wtype is unavailable"))?;
            successful(run(&executable, &["-"], text.as_bytes(), Duration::from_secs(5)).await?)?;
            return Ok(true);
        }
        Ok(false) // Default: Waymote IME commits.
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Wayland text timed out"))?
}
struct Output {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}
fn successful(output: Output) -> io::Result<()> {
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "text helper failed: {}",
            output.status
        )))
    }
}
async fn focused(executable: &std::path::Path) -> io::Result<bool> {
    let result = run(
        executable,
        &["getwindowfocus", "getwindowpid"],
        &[],
        Duration::from_millis(250),
    )
    .await
    .map_err(|e| io::Error::new(e.kind(), format!("X11 focus probe: {e}")))?;
    if !result.status.success() {
        let diagnostic = String::from_utf8_lossy(&result.stderr);
        if result.status.code() == Some(1)
            && [
                "has no pid associated with it.",
                "xdo_focus_window reported an error",
                "XGetInputFocus returned the focused window of 1.",
            ]
            .iter()
            .any(|v| diagnostic.contains(v))
        {
            return Ok(false);
        }
        return Err(io::Error::other(format!(
            "X11 focus probe failed: {}",
            result.status
        )));
    }
    let pid = std::str::from_utf8(&result.stdout)
        .ok()
        .and_then(|v| v.trim().parse::<i32>().ok())
        .ok_or_else(|| io::Error::other("X11 focus probe returned an invalid PID"))?;
    Ok(pid > 0)
}
// The process group also covers descendants retaining stdout/stderr. On cancellation
// kill_on_drop reaps the direct child through Tokio; the group guard kills helpers.
struct Group(i32);
impl Drop for Group {
    fn drop(&mut self) {
        let _ = killpg(Pid::from_raw(self.0), Signal::SIGKILL);
    }
}
async fn read_limited(reader: impl tokio::io::AsyncRead + Unpin) -> io::Result<Vec<u8>> {
    let mut data = Vec::new();
    reader.take(16_385).read_to_end(&mut data).await?;
    if data.len() > 16_384 {
        return Err(io::Error::other("text helper output exceeds limit"));
    }
    Ok(data)
}
async fn run(
    executable: &std::path::Path,
    args: &[&str],
    text: &[u8],
    limit: Duration,
) -> io::Result<Output> {
    let mut child = Command::new(executable)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()?;
    let group = Group(
        child
            .id()
            .ok_or_else(|| io::Error::other("text helper PID unavailable"))? as i32,
    );
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    // Keep I/O in this future, so cancellation drops all pipes without detached tasks.
    let pipes = async {
        let (_, stdout, stderr) = tokio::try_join!(
            async move {
                stdin.write_all(text).await?;
                drop(stdin);
                Ok::<_, io::Error>(())
            },
            read_limited(stdout),
            read_limited(stderr)
        )?;
        Ok::<_, io::Error>((stdout, stderr))
    };
    tokio::pin!(pipes);
    let operation = async {
        // Poll pipes while waiting, avoiding a full stdout/stdin pipe deadlock.
        tokio::select! {
            data = &mut pipes => {
                let (stdout, stderr) = data?;
                Ok(Output {status: child.wait().await?, stdout, stderr})
            }
            status = child.wait() => {
                let status = status?;
                let (stdout, stderr) = tokio::time::timeout(Duration::from_millis(25), &mut pipes).await
                    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "text helper pipes did not close"))??;
                Ok(Output {status, stdout, stderr})
            }
        }
    };
    let result = tokio::time::timeout(limit, operation)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "text helper timed out"))
        .and_then(|v| v);
    drop(group);
    // Always collect the direct child, including on pipe or command timeout.
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_millis(250), child.wait()).await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    // Forked helpers can briefly inherit another test's writable script FD.
    // Serialize fixture writes and execs so ETXTBSY cannot mask delivery errors.
    static FIXTURES: std::sync::Mutex<()> = std::sync::Mutex::new(());
    struct Fixture {
        dir: tempfile::TempDir,
        _guard: std::sync::MutexGuard<'static, ()>,
    }
    impl Fixture {
        fn new() -> Self {
            let guard = FIXTURES.lock().unwrap_or_else(|e| e.into_inner());
            Self {
                dir: tempfile::tempdir().unwrap(),
                _guard: guard,
            }
        }
        fn config(&self, x11: bool, display: bool, wtype: bool) -> Config {
            Config {
                x11,
                display,
                wtype,
                path: self.dir.path().as_os_str().into(),
            }
        }
        fn script(&self, name: &str, body: &str) -> PathBuf {
            let path = self.dir.path().join(name);
            std::fs::write(
                &path,
                format!("#!/bin/sh\ncd '{}'\n{body}\n", self.dir.path().display()),
            )
            .unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }
        fn log(&self, name: &str) -> Vec<u8> {
            std::fs::read(self.dir.path().join(name)).unwrap()
        }
        fn absent(&self, name: &str) {
            assert!(!self.dir.path().join(name).exists());
        }
    }
    #[tokio::test]
    async fn default_and_display_gate_do_not_probe() {
        let f = Fixture::new();
        f.script("xdotool", "echo called > probe; exit 2");
        for (x11, display) in [(false, true), (true, false)] {
            assert!(
                !type_with(&f.config(x11, display, false), "abc")
                    .await
                    .unwrap()
            );
        }
        f.absent("probe");
    }
    #[tokio::test]
    async fn x11_uses_focus_pid_and_utf8_stdin_without_window_target() {
        let f = Fixture::new();
        f.script("xdotool", "if [ \"$1\" = getwindowfocus ]; then printf '%s\\n' \"$@\" > probe; echo 42; else printf '%s\\n' \"$@\" > args; /bin/cat > text; fi");
        f.script("wtype", "echo called > wtype-called; exit 1");
        let text = "--世界🙂\n'$(false)'";
        assert!(type_with(&f.config(true, true, true), text).await.unwrap());
        assert_eq!(f.log("probe"), b"getwindowfocus\ngetwindowpid\n");
        assert_eq!(
            f.log("args"),
            b"type\n--clearmodifiers\n--delay\n1\n--file\n-\n"
        );
        assert_eq!(f.log("text"), text.as_bytes());
        f.absent("wtype-called");
    }
    #[tokio::test]
    async fn absent_pid_and_recognized_focus_errors_allow_fallback() {
        let f = Fixture::new();
        f.script("wtype", "printf '%s\\n' \"$@\" > args; /bin/cat > text");
        for body in [
            "echo 0",
            "echo -1",
            "echo 'window has no pid associated with it.' >&2; exit 1",
            "echo 'xdo_focus_window reported an error' >&2; exit 1",
            "echo 'XGetInputFocus returned the focused window of 1.' >&2; exit 1",
        ] {
            f.script("xdotool", body);
            assert!(
                !type_with(&f.config(true, true, false), "IME")
                    .await
                    .unwrap()
            );
            assert!(
                type_with(&f.config(true, true, true), "世界")
                    .await
                    .unwrap()
            );
            assert_eq!(f.log("text"), "世界".as_bytes());
            assert_eq!(f.log("args"), b"-\n");
        }
    }
    #[tokio::test]
    async fn missing_xdotool_and_independent_wtype() {
        let f = Fixture::new();
        assert!(
            !type_with(&f.config(true, true, false), "IME")
                .await
                .unwrap()
        );
        assert!(
            type_with(&f.config(false, false, true), "hello")
                .await
                .is_err()
        );
        f.script("wtype", "/bin/cat > text");
        for (x11, display) in [(false, false), (true, true)] {
            assert!(
                type_with(&f.config(x11, display, true), "🙂")
                    .await
                    .unwrap()
            );
            assert_eq!(f.log("text"), "🙂".as_bytes());
        }
    }
    #[tokio::test]
    async fn broken_probe_never_falls_back() {
        let f = Fixture::new();
        f.script("wtype", "echo called > fallback");
        for body in [
            "exit 1",
            "echo 'cannot open display' >&2; exit 1",
            "echo 'has no pid associated with it.' >&2; exit 2",
            "echo invalid",
            "echo 2147483648",
            "echo 42 43",
        ] {
            f.script("xdotool", body);
            assert!(
                type_with(&f.config(true, true, true), "text")
                    .await
                    .is_err(),
                "{body}"
            );
            f.absent("fallback");
        }
        let path = f.script("xdotool", "echo 42");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            type_with(&f.config(true, true, true), "text")
                .await
                .is_err()
        );
        f.absent("fallback");
    }
    #[tokio::test]
    async fn partial_delivery_never_retries() {
        let f = Fixture::new();
        f.script("xdotool", "if [ \"$1\" = getwindowfocus ]; then echo 42; else /bin/dd bs=1 count=3 of=prefix 2>/dev/null; exit 7; fi");
        f.script("wtype", "echo called > fallback");
        let failure = type_with(&f.config(true, true, true), "世界🙂")
            .await
            .unwrap_err();
        assert!(failure.to_string().contains("exit status: 7"), "{failure}");
        assert_eq!(f.log("prefix"), "世".as_bytes());
        f.absent("fallback");
        f.script(
            "wtype",
            "/bin/dd bs=1 count=3 of=prefix 2>/dev/null; exit 9",
        );
        let failure = type_with(&f.config(false, false, true), "界🙂")
            .await
            .unwrap_err();
        assert!(failure.to_string().contains("exit status: 9"), "{failure}");
        assert_eq!(f.log("prefix"), "界".as_bytes());
    }
    #[tokio::test]
    async fn probe_and_inherited_pipes_have_deadlines() {
        let f = Fixture::new();
        f.script("wtype", "echo called > fallback");
        for body in ["exec /bin/sleep 10", "/bin/sleep 10 & echo 42"] {
            f.script("xdotool", body);
            let start = std::time::Instant::now();
            assert!(
                type_with(&f.config(true, true, true), "text")
                    .await
                    .is_err()
            );
            assert!(start.elapsed() < Duration::from_secs(2));
            f.absent("fallback");
        }
    }
    #[tokio::test]
    async fn stalled_stdin_and_excess_output_are_bounded_and_reaped() {
        let f = Fixture::new();
        let executable = f.script("stall", "echo $$ > pid; exec /bin/sleep 10");
        let start = std::time::Instant::now();
        assert!(
            run(
                &executable,
                &[],
                &vec![b'x'; 1_000_000],
                Duration::from_millis(100)
            )
            .await
            .is_err()
        );
        assert!(start.elapsed() < Duration::from_secs(2));
        let pid: i32 = String::from_utf8(f.log("pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            nix::sys::signal::kill(Pid::from_raw(pid), None),
            Err(nix::errno::Errno::ESRCH)
        );
        let executable = f.script(
            "output",
            "/bin/dd if=/dev/zero bs=20000 count=1 2>/dev/null",
        );
        assert!(
            run(&executable, &[], &[], Duration::from_millis(250))
                .await
                .is_err()
        );
    }
}
