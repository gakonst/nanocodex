//! Pipe-only child ownership adapter. Dropping a handle kills and reaps its child.
use std::{collections::HashMap, path::Path, process::Stdio, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::{Notify, mpsc, oneshot},
};

pub(crate) struct ProcessHandle {
    writer: mpsc::Sender<Vec<u8>>,
    stop: Arc<Notify>,
}

impl ProcessHandle {
    pub(crate) fn writer_sender(&self) -> &mpsc::Sender<Vec<u8>> {
        &self.writer
    }
    pub(crate) fn terminate(&self) {
        self.stop.notify_one();
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        self.terminate();
    }
}

pub(crate) struct SpawnedProcess {
    pub session: ProcessHandle,
    pub stdout_rx: mpsc::Receiver<Vec<u8>>,
    pub stderr_rx: mpsc::Receiver<Vec<u8>>,
    pub exit_rx: oneshot::Receiver<i32>,
}

pub(crate) async fn spawn_pipe_process(
    path: &Path,
    args: &[String],
    directory: &Path,
    environment: &HashMap<String, String>,
    _arg0: &Option<String>,
    _extra: &[String],
) -> std::io::Result<SpawnedProcess> {
    let mut child = Command::new(path)
        .args(args)
        .current_dir(directory)
        .env_clear()
        .envs(environment)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut input = child.stdin.take().expect("piped input");
    let mut output = child.stdout.take().expect("piped output");
    let mut error = child.stderr.take().expect("piped error");
    let (writer, mut writes) = mpsc::channel::<Vec<u8>>(8);
    let (stdout, stdout_rx) = mpsc::channel(16);
    let (_stderr, stderr_rx) = mpsc::channel(1);
    let (exit, exit_rx) = oneshot::channel();
    let stop = Arc::new(Notify::new());
    let stopped = stop.clone();
    tokio::spawn(async move {
        let writer = tokio::spawn(async move {
            while let Some(frame) = writes.recv().await {
                if input.write_all(&frame).await.is_err() || input.flush().await.is_err() {
                    break;
                }
            }
        });
        let mut reader = tokio::spawn(async move {
            let mut bytes = [0; 8192];
            loop {
                match output.read(&mut bytes).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if stdout.send(bytes[..n].to_vec()).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let drain = tokio::spawn(async move {
            let _ = tokio::io::copy(&mut error, &mut tokio::io::sink()).await;
        });
        let status = tokio::select! {
            biased;
            _ = stopped.notified() => { let _ = child.start_kill(); child.wait().await },
            status = child.wait() => status,
        };
        writer.abort();
        // Preserve a final acknowledgement already written before normal exit.
        if status.as_ref().is_ok_and(|status| status.success()) {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(1), &mut reader).await;
        }
        reader.abort();
        drain.abort();
        let _ = exit.send(status.ok().and_then(|s| s.code()).unwrap_or(-1));
    });
    Ok(SpawnedProcess {
        session: ProcessHandle { writer, stop },
        stdout_rx,
        stderr_rx,
        exit_rx,
    })
}
