//! Platform speaker capture. Encoding and lifetime ownership live in the shared core.
#[cfg(any(target_os = "windows", target_os = "macos"))]
use super::screen_video::Task;
use super::screen_video::{Capture, VideoSource};
use nanocodex_remote::Result;
#[cfg(any(target_os = "windows", target_os = "macos"))]
use nanocodex_remote::audio::FRAME_SAMPLES;
use std::sync::Arc;

pub(crate) fn native_source() -> Option<VideoSource> {
    #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
    {
        Some(Arc::new(|| Box::pin(native_capture())))
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
async fn native_capture() -> Result<Capture> {
    use std::process::Stdio;
    let output = tokio::process::Command::new("pactl")
        .arg("get-default-sink")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await?;
    if !output.status.success() {
        return Err("desktop playback sink unavailable".into());
    }
    let name = std::str::from_utf8(&output.stdout)?.trim();
    let output = tokio::process::Command::new("pactl")
        .args(["--format=json", "list", "sinks"])
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await?;
    if !output.status.success() {
        return Err("desktop playback monitors unavailable".into());
    }
    let monitor = monitor_source(&output.stdout, name)?;
    let child = tokio::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-f",
            "pulse",
            "-fragment_size",
            "3840",
            "-i",
            &monitor,
            "-vn",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-flush_packets",
            "1",
            "-f",
            "s16le",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    Capture::child(child)
}

#[cfg(any(target_os = "linux", test))]
fn monitor_source(data: &[u8], name: &str) -> Result<String> {
    let sinks: Vec<serde_json::Value> = serde_json::from_slice(data)?;
    sinks
        .iter()
        .find(|sink| !name.is_empty() && sink["name"] == name)
        .and_then(|sink| {
            sink["monitor_source_name"]
                .as_str()
                .or_else(|| sink["monitor_source"].as_str())
        })
        .filter(|monitor| !monitor.is_empty() && monitor.len() <= 4096)
        .map(str::to_owned)
        .ok_or_else(|| "desktop playback monitor unavailable".into())
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
async fn native_capture() -> Result<Capture> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::AsyncWriteExt;
    struct Stop(Arc<AtomicBool>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    struct Writer {
        pipe: tokio::io::DuplexStream,
        runtime: tokio::runtime::Handle,
    }
    impl std::io::Write for Writer {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.runtime.block_on(self.pipe.write(data))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (reader, pipe) = tokio::io::duplex(FRAME_SAMPLES * 2 * 4);
    let stop = Arc::new(AtomicBool::new(false));
    let guard = Stop(stop.clone());
    let runtime = tokio::runtime::Handle::current();
    let thread = tokio::task::spawn_blocking(move || {
        nanocodex_hand::capture_audio(Writer { pipe, runtime }, stop)
    });
    Ok(Capture {
        reader: Box::new(reader),
        owner: Task(tokio::spawn(async move {
            let _stop = guard;
            match thread.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::warn!(%error, "desktop audio source stopped"),
                Err(error) => tracing::warn!(%error, "desktop audio worker stopped"),
            }
        })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn playback_monitor_never_falls_back_to_microphone() {
        let data = br#"[{"name":"speaker","monitor_source_name":"speaker.monitor"}]"#;
        assert_eq!(monitor_source(data, "speaker").unwrap(), "speaker.monitor");
        for name in ["", "default", "microphone"] {
            assert!(monitor_source(data, name).is_err());
        }
        assert!(monitor_source(br#"[{"name":"speaker"}]"#, "speaker").is_err());
    }
}
