//! Live compositor adapter for the shared publisher. One Waymote process owns
//! capture and input across signaling reconnects; viewers receive complete frames.
use super::{
    screen_gamepad::Controller,
    screen_publisher::ScreenBackend,
    screen_video::{Capture, Task, VideoSource},
    screen_wayland_input::{Input, record},
};
use nanocodex_managed::ManagedError;
use serde_json::{Value, json};
use std::{
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{Mutex, broadcast, watch},
};
#[path = "screen_wayland_frames.rs"]
mod frames;
#[path = "screen_wayland_text.rs"]
mod text;
type Result<T> = std::result::Result<T, ManagedError>;
fn error(e: impl std::fmt::Display) -> ManagedError {
    ManagedError::Configuration(e.to_string())
}
struct InputPipe {
    pipe: Option<tokio::process::ChildStdin>,
    sequence: u32,
}
struct State {
    input: Mutex<InputPipe>,
    gamepad: Controller,
    frames: broadcast::Sender<Arc<Vec<u8>>>,
    alive: AtomicBool,
}
pub(crate) struct Platform {
    state: Arc<State>,
    stop: watch::Sender<bool>,
    worker: Option<tokio::task::JoinHandle<()>>,
}
impl Platform {
    pub(crate) async fn start() -> Result<Self> {
        let bitrate = std::env::var("NANOCODEX_SCREEN_BITRATE_KBPS")
            .unwrap_or("6000".into())
            .parse::<u32>()
            .map_err(error)?;
        if !(1000..=100000).contains(&bitrate) {
            return Err(error("screen bitrate must be 1000 through 100000"));
        }
        let executable =
            std::env::var_os("NANOCODEX_WAYMOTE").unwrap_or_else(|| "waymote-streamd".into());
        let mut command = tokio::process::Command::new(executable);
        command
            .args([
                "--frame-rate",
                "60",
                "--bitrate",
                &bitrate.to_string(),
                "--xkb-layout",
                "us",
                "--ffmpeg",
            ])
            .arg(std::env::current_exe().map_err(error)?)
            .env(super::screen_wayland_encoder::HELPER_ENV, "1")
            // This pipe is always framed. Annex-B remains available for direct helper use.
            .env_remove("NANOCODEX_SCREEN_FRAME_BOUNDARIES")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .process_group(0);
        Self::start_command(command).await
    }
    async fn start_command(mut command: tokio::process::Command) -> Result<Self> {
        let mut child = command.spawn().map_err(error)?;
        let group =
            ProcessGroup(child.id().ok_or_else(|| error("Waymote PID unavailable"))? as i32);
        let input = child
            .stdin
            .take()
            .ok_or_else(|| error("Waymote input unavailable"))?;
        let reader = child
            .stdout
            .take()
            .ok_or_else(|| error("Waymote video unavailable"))?;
        let (sender, _) = broadcast::channel(8);
        let state = Arc::new(State {
            input: Mutex::new(InputPipe {
                pipe: Some(input),
                sequence: 0,
            }),
            gamepad: Controller::configured(),
            frames: sender,
            alive: AtomicBool::new(false),
        });
        let (stop, mut stopped) = watch::channel(false);
        let (ready, mut readiness) = watch::channel(false);
        let running = state.clone();
        let worker = tokio::spawn(async move {
            let mut group = group;
            tokio::select! {
                _=stopped.changed()=>{},
                result=frames::read(reader,|frame| {
                    running.alive.store(true,Ordering::Release);
                    ready.send_replace(true);
                    let _=running.frames.send(Arc::new(frame));
                })=>{if let Err(e)=result {eprintln!("Wayland video forwarding failed: {e}");}},
                _=child.wait()=>{},
            }
            running.alive.store(false, Ordering::Release);
            let _ = running.release().await;
            running.input.lock().await.pipe.take();
            // Kill the process group before waiting: helpers may otherwise keep pipes open.
            group.kill();
            let _ = child.wait().await;
        });
        let platform = Self {
            state,
            stop,
            worker: Some(worker),
        };
        if tokio::time::timeout(Duration::from_secs(15), readiness.wait_for(|v| *v))
            .await
            .is_err()
            || !platform.state.alive.load(Ordering::Acquire)
        {
            platform.shutdown().await;
            return Err(error("Wayland capture did not produce a frame"));
        }
        Ok(platform)
    }
    pub(crate) fn backend(&self) -> ScreenBackend {
        let state = self.state.clone();
        Arc::new(move |input| {
            let state = state.clone();
            Box::pin(async move {
                match input["action"].as_str() {
                    Some("release") => {
                        state.release().await?;
                        Ok(json!({"status":"ok"}))
                    }
                    Some("capabilities") => Ok(
                        json!({"status":"ok","relativePointer":state.alive.load(Ordering::Acquire),"gamepad":state.alive.load(Ordering::Acquire)&&state.gamepad.available()}),
                    ),
                    Some("observe") if state.alive.load(Ordering::Acquire) => snapshot().await,
                    Some("input") if state.alive.load(Ordering::Acquire) => {
                        let event = Input::parse(input["input"].clone()).map_err(error)?;
                        state.apply(event).await?;
                        Ok(json!({"status":"ok"}))
                    }
                    _ => Ok(json!({"status":"unavailable"})),
                }
            })
        })
    }
    pub(crate) fn video(&self) -> VideoSource {
        let state = self.state.clone();
        Arc::new(move || {
            let state = state.clone();
            Box::pin(async move {
                if !state.alive.load(Ordering::Acquire) {
                    return Err("Wayland capture stopped".into());
                }
                let mut frames = state.frames.subscribe();
                let (reader, mut writer) = tokio::io::duplex(256 * 1024);
                let owner = Task(tokio::spawn(async move {
                    if writer.write_all(b"NCH264F1").await.is_err() {
                        return;
                    }
                    let mut need_keyframe = true;
                    loop {
                        if !state.alive.load(Ordering::Acquire) {
                            break;
                        }
                        let received =
                            match tokio::time::timeout(Duration::from_secs(1), frames.recv()).await
                            {
                                Ok(v) => v,
                                Err(_) => continue,
                            };
                        let frame = match received {
                            Ok(v) => v,
                            Err(broadcast::error::RecvError::Lagged(_)) => {
                                need_keyframe = true;
                                continue;
                            }
                            Err(_) => break,
                        };
                        if need_keyframe && !frames::keyframe(&frame) {
                            continue;
                        }
                        need_keyframe = false;
                        if writer
                            .write_all(&(frame.len() as u32).to_be_bytes())
                            .await
                            .is_err()
                            || writer.write_all(&frame).await.is_err()
                        {
                            break;
                        }
                    }
                }));
                Ok(Capture {
                    reader: Box::new(reader),
                    owner,
                })
            })
        })
    }
    pub(crate) async fn shutdown(mut self) {
        let _ = self.state.release().await;
        self.stop.send_replace(true);
        if let Some(mut worker) = self.worker.take()
            && tokio::time::timeout(Duration::from_secs(3), &mut worker)
                .await
                .is_err()
        {
            worker.abort();
            let _ = worker.await;
        }
    }
}
impl Drop for Platform {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}
struct ProcessGroup(i32);
impl ProcessGroup {
    fn kill(&mut self) {
        if self.0 > 0 {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(self.0),
                nix::sys::signal::Signal::SIGKILL,
            );
            // Disarm before waiting/reaping so Drop cannot signal a reused PID.
            self.0 = 0;
        }
    }
}
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}
impl State {
    async fn apply(&self, event: Input) -> Result<()> {
        // Serialize all input, including gamepad snapshots and releases.
        let mut input = self.input.lock().await;
        if let Input::Gamepad { gamepad } = &event {
            return self.gamepad.apply(gamepad).map_err(error);
        }
        if let Input::Text { text } = &event
            && self::text::type_text(text).await.map_err(error)?
        {
            return Ok(());
        }
        let release = matches!(event, Input::ReleaseAll {});
        let gamepad = if release {
            self.gamepad.release().map_err(error)
        } else {
            Ok(())
        };
        let bytes = event.records(&mut input.sequence);
        let result = write(&mut input, &bytes).await;
        gamepad.and(result)
    }
    async fn release(&self) -> Result<()> {
        let mut input = self.input.lock().await;
        let gamepad = self.gamepad.release().map_err(error);
        let result = if input.pipe.is_some() {
            write(&mut input, &record(5, 0, 0, 0, 0)).await
        } else {
            Ok(())
        };
        gamepad.and(result)
    }
}
async fn write(input: &mut InputPipe, bytes: &[u8]) -> Result<()> {
    let pipe = input
        .pipe
        .as_mut()
        .ok_or_else(|| error("Wayland input closed"))?;
    match tokio::time::timeout(Duration::from_millis(250), pipe.write_all(bytes)).await {
        Ok(Ok(())) => Ok(()),
        result => {
            // A partial record cannot be safely retried. Closing stdin asks Waymote
            // to release input and prevents later records completing a corrupt event.
            input.pipe.take();
            Err(error(format!("Wayland input pipe failed: {result:?}")))
        }
    }
}
async fn snapshot() -> Result<Value> {
    use base64::Engine;
    // grim's output scale is independent of physical monitor resolution. Decode
    // with image limits, then enforce the shared screenshot size/transport budget.
    let mut child = tokio::process::Command::new("grim")
        .args(["-t", "png", "-l", "1", "-s", "0.5", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(error)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| error("grim stdout unavailable"))?;
    let captured = tokio::time::timeout(Duration::from_secs(3), async {
        let mut bytes = Vec::new();
        stdout.take(8_000_001).read_to_end(&mut bytes).await?;
        if bytes.len() > 8_000_000 {
            return Err(std::io::Error::other("screenshot exceeds limit"));
        }
        if !child.wait().await?.success() {
            return Err(std::io::Error::other("grim capture failed"));
        }
        Ok(bytes)
    })
    .await
    .map_err(error)?
    .map_err(error)?;
    tokio::task::spawn_blocking(move || {
        let mut reader=image::ImageReader::new(std::io::Cursor::new(captured)).with_guessed_format().map_err(error)?;
        let mut limits=image::Limits::default();limits.max_image_width=Some(16384);limits.max_image_height=Some(16384);limits.max_alloc=Some(256*1024*1024);reader.limits(limits);
        let frame=reader.decode().map_err(error)?.thumbnail(1280,1280);
        for quality in [65,50,35,20,10] {
            let mut jpeg=Vec::new();image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg,quality).encode_image(&frame).map_err(error)?;
            let encoded=base64::engine::general_purpose::STANDARD.encode(jpeg);
            if encoded.len()<=500000 {return Ok(json!({"status":"ok","jpeg":encoded,"width":frame.width(),"height":frame.height(),"inputKeepalive":false}));}
        }
        Err(error("screenshot exceeds transport limit"))
    }).await.map_err(error)?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn real_pipe_lifecycle_forwards_capture_serializes_input_and_releases() {
        // No compositor/device dependency: exercise the real process pipes and
        // CaptureSource/backend API using a synthetic Waymote process.
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("waymote.py");
        let log = directory.path().join("input.bin");
        std::fs::write(
            &script,
            r#"import os,sys,threading,time
log=open(sys.argv[1],'wb',buffering=0)
def inputs():
 while True:
  value=os.read(0,4096)
  if not value: return
  log.write(value)
threading.Thread(target=inputs,daemon=True).start()
os.write(1,b'NCH264C1')
while True:
 os.write(1,b'\x80\x00\x00\x05\x00\x00\x00\x01\x65')
 time.sleep(0.02)
"#,
        )
        .unwrap();
        let mut command = tokio::process::Command::new("python3");
        command
            .arg(script)
            .arg(&log)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .process_group(0);
        let platform = Platform::start_command(command).await.unwrap();
        let backend = platform.backend();
        let capabilities = backend(json!({"action":"capabilities"})).await.unwrap();
        assert_eq!(capabilities["relativePointer"], true);
        let mut capture = platform.video()().await.unwrap();
        let mut bytes = [0; 17];
        tokio::time::timeout(
            Duration::from_secs(1),
            capture.reader.read_exact(&mut bytes),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(&bytes[..8], b"NCH264F1");
        assert_eq!(&bytes[8..], b"\0\0\0\x05\0\0\0\x01\x65");
        backend(json!({"action":"input","input":{"kind":"key","key":4,"down":true}}))
            .await
            .unwrap();
        backend(json!({"action":"input","input":{"kind":"relativeMove","deltaX":12,"deltaY":-4}}))
            .await
            .unwrap();
        backend(json!({"action":"release"})).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if std::fs::metadata(&log).unwrap().len() >= 48 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let inputs = std::fs::read(log).unwrap();
        assert_eq!(&inputs[..16], &record(4, 1, 30, 0, 1));
        assert_eq!(
            &inputs[16..32],
            &record(8, 0, 12f32.to_bits(), (-4f32).to_bits(), 2)
        );
        assert_eq!(&inputs[32..48], &record(5, 0, 0, 0, 0));
        platform.shutdown().await;
        assert_eq!(
            backend(json!({"action":"capabilities"})).await.unwrap()["relativePointer"],
            false
        );
        assert_eq!(
            backend(json!({"action":"input","input":{"kind":"key","key":4,"down":true}}))
                .await
                .unwrap()["status"],
            "unavailable"
        );
    }
}
