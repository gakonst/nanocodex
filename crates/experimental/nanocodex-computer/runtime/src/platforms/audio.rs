//! Explicit Linux PulseAudio/PipeWire monitor capture using supplied pactl/ffmpeg.
use super::process;
use crate::{Error, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    path::PathBuf,
    process::Child,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
struct Recording {
    child: Child,
    path: PathBuf,
    ready: Arc<AtomicBool>,
    stderr: Arc<Mutex<Vec<u8>>>,
    reader: Option<JoinHandle<()>>,
}
impl Recording {
    fn stop_child(&mut self) {
        process::kill_tree(&mut self.child);
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
impl Drop for Recording {
    fn drop(&mut self) {
        self.stop_child();
    }
}
pub struct Audio {
    pactl: PathBuf,
    ffmpeg: PathBuf,
    directory: PathBuf,
    active: Option<Recording>,
    retained: Vec<PathBuf>,
}
impl Audio {
    pub fn new(pactl: PathBuf, ffmpeg: PathBuf, directory: PathBuf) -> Result<Self> {
        if !directory.is_absolute() {
            return Err(Error::invalid("Audio directory must be absolute"));
        }
        if !directory.exists() {
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(&directory)?;
        }
        let metadata = fs::symlink_metadata(&directory)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(Error::invalid(
                "Audio directory must be a non-symlink directory",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
                return Err(Error::invalid(
                    "Audio directory must be private and owned by current user",
                ));
            }
        }
        Ok(Self {
            pactl,
            ffmpeg,
            directory,
            active: None,
            retained: vec![],
        })
    }
    pub fn execute(&mut self, method: &str, params: &Value) -> Result<Value> {
        match method {
            "capabilities" => Ok(
                json!({"target":"linux-audio","methods":["start_audio_recording","stop_audio_recording","status","clear"],"format":{"sample_rate":24000,"channels":2,"codec":"pcm_s16le","container":"wav"}}),
            ),
            "start_audio_recording" => {
                self.start(super::unsigned(params, "max_duration_ms", 60000)?)
            }
            "stop_audio_recording" => self.stop(),
            "status" => Ok(
                json!({"active":self.active.is_some(),"retained_recordings":self.retained.len()}),
            ),
            "clear" => {
                if self.active.is_some() {
                    return Err(Error::action("Cannot clear while recording"));
                }
                let count = self.retained.len();
                for path in &self.retained {
                    if path.exists() {
                        fs::remove_file(path)?;
                    }
                }
                self.retained.clear();
                Ok(json!({"removed":count}))
            }
            _ => Err(Error::unsupported(format!(
                "Unknown audio method: {method}"
            ))),
        }
    }
    fn start(&mut self, max_duration_ms: u64) -> Result<Value> {
        if self.active.is_some() {
            return Err(Error::action("computer audio recording is already active"));
        }
        if !(100..=300000).contains(&max_duration_ms) {
            return Err(Error::invalid(
                "max_duration_ms must be an integer from 100 through 300000",
            ));
        }
        let sink = process::run(
            &self.pactl,
            &["get-default-sink".into()],
            &[],
            Duration::from_secs(5),
        )?;
        let sink = std::str::from_utf8(&sink)
            .map_err(|_| Error::action("Invalid pactl sink encoding"))?
            .trim();
        if sink.is_empty() || sink.len() > 4096 || sink.contains('\0') {
            return Err(Error::action("pactl returned an invalid default sink"));
        }
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = self
            .directory
            .join(format!("recording-{}-{nanos}.wav", std::process::id()));
        let args = vec![
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
            "-n".into(),
            "-progress".into(),
            "pipe:2".into(),
            "-f".into(),
            "pulse".into(),
            "-i".into(),
            format!("{sink}.monitor"),
            "-t".into(),
            format!("{}", max_duration_ms as f64 / 1000.),
            "-ar".into(),
            "24000".into(),
            "-ac".into(),
            "2".into(),
            "-c:a".into(),
            "pcm_s16le".into(),
            path.to_string_lossy().into_owned(),
        ];
        let mut child = process::spawn(&self.ffmpeg, &args)?;
        child.stdout.take();
        let mut stderr = child.stderr.take().unwrap();
        let ready = Arc::new(AtomicBool::new(false));
        let log = Arc::new(Mutex::new(Vec::new()));
        let ready_clone = ready.clone();
        let log_clone = log.clone();
        let reader = thread::spawn(move || {
            let mut block = [0; 4096];
            while let Ok(count) = stderr.read(&mut block) {
                if count == 0 {
                    break;
                }
                let mut log = log_clone.lock().unwrap();
                log.extend_from_slice(&block[..count]);
                if log
                    .windows(b"progress=continue".len())
                    .any(|w| w == b"progress=continue")
                {
                    ready_clone.store(true, Ordering::SeqCst);
                }
                if log.len() > 65536 {
                    let excess = log.len() - 65536;
                    log.drain(..excess);
                }
            }
        });
        let mut recording = Recording {
            child,
            path: path.clone(),
            ready,
            stderr: log,
            reader: Some(reader),
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let result = (|| -> Result<()> {
            loop {
                if recording.ready.load(Ordering::SeqCst) {
                    return Ok(());
                }
                if let Some(status) = recording.child.try_wait()? {
                    return Err(Error::action(format!(
                        "Audio capture stopped before startup: {status}"
                    )));
                }
                if Instant::now() >= deadline {
                    return Err(Error::new(-32008, "Audio capture startup timed out"));
                }
                thread::sleep(Duration::from_millis(5));
            }
        })();
        if let Err(error) = result {
            recording.stop_child();
            let _ = fs::remove_file(path);
            return Err(error);
        }
        self.active = Some(recording);
        Ok(json!({"recording":true,"max_duration_ms":max_duration_ms}))
    }
    fn stop(&mut self) -> Result<Value> {
        let mut recording = self
            .active
            .take()
            .ok_or_else(|| Error::action("computer audio recording is not active"))?;
        let result = (|| -> Result<Value> {
            if recording.child.try_wait()?.is_none()
                && let Some(mut stdin) = recording.child.stdin.take()
            {
                stdin.write_all(b"q\n")?;
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            let status = loop {
                if let Some(status) = recording.child.try_wait()? {
                    break status;
                }
                if Instant::now() >= deadline {
                    return Err(Error::new(-32008, "Audio stop timed out"));
                }
                thread::sleep(Duration::from_millis(5));
            };
            recording.stop_child();
            if !status.success() {
                let message = recording
                    .stderr
                    .lock()
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_default();
                return Err(Error::action(format!(
                    "Audio capture failed: {}",
                    message.trim()
                )));
            }
            let metadata = fs::symlink_metadata(&recording.path)?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() > 32 * 1024 * 1024
            {
                return Err(Error::action(
                    "Audio output must be a regular WAV file within 32MiB",
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&recording.path, fs::Permissions::from_mode(0o600))?;
            }
            let bytes = fs::read(&recording.path)?;
            if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
                return Err(Error::action("Audio capture did not return WAV"));
            }
            Ok(
                json!({"filepath":recording.path,"mime_type":"audio/wav","data":STANDARD.encode(bytes)}),
            )
        })();
        if result.is_ok() {
            self.retained.push(recording.path.clone());
        } else {
            recording.stop_child();
            let _ = fs::remove_file(&recording.path);
        }
        result
    }
    pub fn cancel(&mut self) -> Result<()> {
        if let Some(mut recording) = self.active.take() {
            recording.stop_child();
            if recording.path.exists() {
                fs::remove_file(&recording.path)?;
            }
        }
        Ok(())
    }
    pub fn end_turn(&mut self) -> Result<()> {
        if self.active.is_some() {
            self.stop()?;
        }
        Ok(())
    }
}
impl Drop for Audio {
    fn drop(&mut self) {
        if let Some(mut recording) = self.active.take() {
            recording.stop_child();
            let _ = fs::remove_file(&recording.path);
        }
    }
}
