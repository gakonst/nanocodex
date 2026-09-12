//! Bounded JSON subprocess adapter for explicitly configured local broker/reviewer
//! programs. No command string or shell is involved; stderr and secrets are never
//! forwarded to the public tool result.
use crate::{Error, Result};
use serde_json::Value;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use zeroize::Zeroize;

#[derive(Clone)]
pub struct Program {
    path: PathBuf,
    timeout: Duration,
}
impl Program {
    pub fn new(path: impl AsRef<Path>, timeout: Duration) -> Result<Self> {
        let path = path.as_ref();
        if !path.is_absolute() {
            return Err(Error::invalid(
                "Broker/reviewer executable must be an absolute path",
            ));
        }
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_file() {
            return Err(Error::invalid(
                "Broker/reviewer must be a regular executable",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let uid = unsafe { libc::geteuid() };
            if metadata.uid() != uid || metadata.mode() & 0o022 != 0 || metadata.mode() & 0o100 == 0
            {
                return Err(Error::invalid(
                    "Broker/reviewer must be owned by this user, executable, and not group/world writable",
                ));
            }
        }
        Ok(Self {
            path: path.to_owned(),
            timeout: timeout.min(Duration::from_secs(120)),
        })
    }
    pub fn request(&self, value: &Value) -> Result<Value> {
        self.request_until(value, Instant::now() + self.timeout)
    }
    /// A caller's operation budget also covers subprocess setup and both pipes.
    /// It cannot increase this trusted program's configured maximum duration.
    pub fn request_until(&self, value: &Value, deadline: Instant) -> Result<Value> {
        let deadline = deadline.min(Instant::now() + self.timeout);
        if Instant::now() >= deadline {
            return Err(Error::action("Broker/reviewer timed out"));
        }
        let mut bytes = serde_json::to_vec(value)?;
        if bytes.len() > 1024 * 1024 {
            return Err(Error::invalid("Broker request exceeds 1 MiB"));
        }
        bytes.push(b'\n');
        if Instant::now() >= deadline {
            bytes.zeroize();
            return Err(Error::action("Broker/reviewer timed out"));
        }
        let mut command = Command::new(&self.path);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| Error::action("Configured broker/reviewer could not start"))?;
        let mut input = child.stdin.take().unwrap();
        let mut output = child.stdout.take().unwrap();
        let writer = std::thread::spawn(move || {
            let result = input.write_all(&bytes);
            bytes.zeroize();
            result
        });
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = (&mut output).take(1024 * 1024 + 1).read_to_end(&mut bytes);
            (result, bytes)
        });
        let pid = child.id();
        let status = loop {
            if let Some(status) = child.try_wait().ok().flatten() {
                break Some(status);
            }
            if Instant::now() >= deadline {
                #[cfg(unix)]
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        // Explicitly configured programs must not leave inherited pipe holders.
        // Readers run independently, so a misbehaving descendant cannot block the
        // service thread after the executable's timeout.
        let join_deadline = Instant::now() + Duration::from_millis(100);
        while (!reader.is_finished() || !writer.is_finished()) && Instant::now() < join_deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        if !reader.is_finished() || !writer.is_finished() {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
            return Err(Error::action("Broker/reviewer left an open pipe"));
        }
        let wrote = writer
            .join()
            .map_err(|_| Error::action("Broker writer failed"))?;
        let (read, mut bytes) = reader
            .join()
            .map_err(|_| Error::action("Broker reader failed"))?;
        let result = if status.is_none() {
            Err(Error::action("Broker/reviewer timed out"))
        } else if !status.is_some_and(|status| status.success()) || wrote.is_err() || read.is_err()
        {
            Err(Error::action("Broker/reviewer failed"))
        } else if bytes.len() > 1024 * 1024 {
            Err(Error::action("Broker/reviewer output exceeds 1 MiB"))
        } else {
            serde_json::from_slice(&bytes)
                .map_err(|_| Error::action("Broker/reviewer returned invalid JSON"))
        };
        bytes.zeroize();
        result
    }
}
