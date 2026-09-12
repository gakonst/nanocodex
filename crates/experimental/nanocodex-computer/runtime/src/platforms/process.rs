use crate::{Error, Result};
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
pub const MAX_OUTPUT: usize = 8 * 1024 * 1024;
fn bounded_read(mut input: impl Read) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    input
        .by_ref()
        .take((MAX_OUTPUT + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_OUTPUT {
        Err(std::io::Error::other("Helper output exceeds 8MiB"))
    } else {
        Ok(bytes)
    }
}
/// One process per command, concurrent pipe drains and a hard execution deadline.
pub fn run(executable: &Path, args: &[String], input: &[u8], timeout: Duration) -> Result<Vec<u8>> {
    if input.len() > MAX_OUTPUT {
        return Err(Error::invalid("Helper input exceeds 8MiB"));
    }
    let mut child = spawn(executable, args)?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let bytes = input.to_vec();
    let output = thread::spawn(move || bounded_read(stdout));
    let error = thread::spawn(move || bounded_read(stderr));
    let writer = thread::spawn(move || stdin.write_all(&bytes));
    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() >= timeout {
            timed_out = true;
            kill_tree(&mut child);
            break child.wait()?;
        }
        thread::sleep(Duration::from_millis(2));
    };
    // Close descendant-held pipes before joining readers and writers.
    kill_tree(&mut child);
    let _ = writer.join();
    let bytes = output
        .join()
        .map_err(|_| Error::action("Helper reader panicked"))??;
    let errors = error
        .join()
        .map_err(|_| Error::action("Helper error reader panicked"))??;
    if timed_out {
        return Err(Error::new(-32008, "Helper process timed out"));
    }
    if !status.success() {
        let text = String::from_utf8_lossy(&errors);
        return Err(Error::action(if text.trim().is_empty() {
            format!("Helper exited with {status}")
        } else {
            text.trim().to_string()
        }));
    }
    Ok(bytes)
}
type WriteJob = (Vec<u8>, mpsc::SyncSender<std::io::Result<()>>);
pub struct Lines {
    child: Child,
    stdin: Option<mpsc::SyncSender<WriteJob>>,
    rx: Receiver<Result<Value>>,
    overflow: Arc<AtomicBool>,
    stderr: Arc<Mutex<Vec<u8>>>,
    threads: Vec<JoinHandle<()>>,
}
impl Lines {
    pub fn spawn(executable: &Path, args: &[String]) -> Result<Self> {
        let mut child = spawn(executable, args)?;
        let mut stdin = child.stdin.take().unwrap();
        let (writer_tx, writer_rx) = mpsc::sync_channel::<WriteJob>(1);
        let writer = thread::spawn(move || {
            while let Ok((bytes, reply)) = writer_rx.recv() {
                let result = stdin.write_all(&bytes).and_then(|_| stdin.flush());
                let failed = result.is_err();
                let _ = reply.send(result);
                if failed {
                    break;
                }
            }
        });
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (tx, rx) = mpsc::sync_channel(256);
        let overflow = Arc::new(AtomicBool::new(false));
        let over = overflow.clone();
        let reader = thread::spawn(move || {
            let mut input = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                let result = match input
                    .by_ref()
                    .take((MAX_OUTPUT + 1) as u64)
                    .read_until(b'\n', &mut line)
                {
                    Ok(0) => break,
                    Ok(_) => {
                        if line.len() > MAX_OUTPUT {
                            Err(Error::action("Helper JSON line exceeds 8MiB"))
                        } else {
                            serde_json::from_slice(&line).map_err(Error::from)
                        }
                    }
                    Err(e) => Err(e.into()),
                };
                let fatal = result.is_err();
                match tx.try_send(result) {
                    Ok(()) => (),
                    Err(TrySendError::Full(_)) => {
                        over.store(true, Ordering::SeqCst);
                        break;
                    }
                    Err(TrySendError::Disconnected(_)) => break,
                }
                if fatal {
                    break;
                }
            }
        });
        let log = Arc::new(Mutex::new(Vec::new()));
        let log_clone = log.clone();
        let errors = thread::spawn(move || {
            let mut stderr = stderr;
            let mut buffer = [0; 4096];
            while let Ok(size) = stderr.read(&mut buffer) {
                if size == 0 {
                    break;
                }
                if let Ok(mut bytes) = log_clone.lock() {
                    bytes.extend_from_slice(&buffer[..size]);
                    if bytes.len() > 65536 {
                        let extra = bytes.len() - 65536;
                        bytes.drain(..extra);
                    }
                }
            }
        });
        Ok(Self {
            child,
            stdin: Some(writer_tx),
            rx,
            overflow,
            stderr: log,
            threads: vec![reader, errors, writer],
        })
    }
    pub fn send(&mut self, request: &Value, timeout: Duration) -> Result<()> {
        let mut bytes = serde_json::to_vec(request)?;
        if bytes.len() > MAX_OUTPUT {
            return Err(Error::invalid("Helper request exceeds 8MiB"));
        }
        bytes.push(b'\n');
        let (reply, answer) = mpsc::sync_channel(1);
        self.stdin
            .as_ref()
            .ok_or_else(|| Error::action("Helper writer closed"))?
            .try_send((bytes, reply))
            .map_err(|_| Error::action("Helper writer is unavailable"))?;
        answer.recv_timeout(timeout).map_err(|e| {
            if matches!(e, mpsc::RecvTimeoutError::Timeout) {
                Error::new(-32008, "Helper request write timed out")
            } else {
                Error::action("Helper writer disconnected")
            }
        })??;
        Ok(())
    }
    pub fn receive(&mut self, timeout: Duration) -> Result<Value> {
        if self.overflow.load(Ordering::SeqCst) {
            return Err(Error::action("Helper event queue overflow"));
        }
        match self.rx.recv_timeout(timeout) {
            Ok(value) => value,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err(Error::new(-32008, "Helper request timed out"))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let status = self.child.try_wait()?;
                let error = self
                    .stderr
                    .lock()
                    .map(|b| String::from_utf8_lossy(&b).to_string())
                    .unwrap_or_default();
                let code = if status.is_some_and(|s| s.code() == Some(130)) {
                    -32010
                } else {
                    -32000
                };
                Err(Error::new(
                    code,
                    format!("Helper disconnected ({status:?}): {}", error.trim()),
                ))
            }
        }
    }
    pub fn close(&mut self) {
        self.stdin.take();
        kill_tree(&mut self.child);
        let _ = self.child.wait();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}
impl Drop for Lines {
    fn drop(&mut self) {
        self.close();
    }
}

pub(crate) fn spawn(executable: &Path, args: &[String]) -> Result<Child> {
    let mut command = Command::new(executable);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000200);
    }
    Ok(command.spawn()?)
}
pub(crate) fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill.exe")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
}
