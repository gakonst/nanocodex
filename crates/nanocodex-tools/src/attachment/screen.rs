//! Native capture commands stay local; no capture command or device is supplied by viewers.
use super::{
    AttachmentError, MAX_OBSERVATION_IMAGE_BYTES, ObservationFrame, ObservationImageFormat,
    ObservationKind, ObservationProvider, ObservationSurface,
};
use std::{
    process::Stdio,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{io::AsyncReadExt as _, process::Command};

/// Native desktop or explicitly selected Android capture provider.
/// Requires FFmpeg, plus screencapture (macOS), grim (Wayland), or adb (Android).
pub struct ScreenObservation {
    surfaces: [ObservationSurface; 1],
    device: Option<String>,
}

impl ScreenObservation {
    /// Shares this process's desktop. OS screen-recording permission is required.
    ///
    /// # Errors
    /// Rejects invalid display names. Capture errors are reported per request.
    pub fn desktop(name: impl Into<String>) -> Result<Self, AttachmentError> {
        Ok(Self {
            surfaces: [ObservationSurface::new(
                "screen",
                name,
                ObservationKind::Desktop,
            )?],
            device: None,
        })
    }

    /// Shares exactly one Android device, identified by its adb serial.
    ///
    /// # Errors
    /// Rejects empty, option-like, or oversized serials and invalid display names.
    pub fn android(
        device: impl Into<String>,
        name: impl Into<String>,
    ) -> Result<Self, AttachmentError> {
        let device = device.into();
        if device.is_empty()
            || device.starts_with('-')
            || device.len() > 256
            || device.contains('\0')
        {
            return Err(failure());
        }
        Ok(Self {
            surfaces: [ObservationSurface::new(
                "screen",
                name,
                ObservationKind::Phone,
            )?],
            device: Some(device),
        })
    }
}

#[async_trait::async_trait]
impl ObservationProvider for ScreenObservation {
    fn surfaces(&self) -> &[ObservationSurface] {
        &self.surfaces
    }

    async fn capture(&self, surface_id: &str) -> Result<ObservationFrame, AttachmentError> {
        if surface_id != "screen" {
            return Err(failure());
        }
        let directory = tempfile::tempdir().map_err(|_| failure())?;
        let raw = directory.path().join("raw.png");
        let mut ffmpeg = Command::new("ffmpeg");
        ffmpeg.args(["-nostdin", "-loglevel", "error"]);
        if let Some(device) = &self.device {
            let bytes = output(
                Command::new("adb").args(["-s", device, "exec-out", "screencap", "-p"]),
                32 * 1024 * 1024,
            )
            .await?;
            tokio::fs::write(&raw, bytes).await.map_err(|_| failure())?;
            ffmpeg.arg("-i").arg(&raw);
        } else if cfg!(target_os = "macos") {
            output(
                Command::new("screencapture")
                    .args(["-x", "-t", "png"])
                    .arg(&raw),
                1024,
            )
            .await?;
            ffmpeg.arg("-i").arg(&raw);
        } else if cfg!(target_os = "windows") {
            ffmpeg.args(["-f", "gdigrab", "-i", "desktop"]);
        } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            output(Command::new("grim").arg(&raw), 1024).await?;
            ffmpeg.arg("-i").arg(&raw);
        } else if let Some(display) = std::env::var_os("DISPLAY") {
            ffmpeg.args(["-f", "x11grab", "-i"]).arg(display);
        } else {
            return Err(failure());
        }
        ffmpeg.args([
            "-frames:v",
            "1",
            "-vf",
            "scale=960:960:force_original_aspect_ratio=decrease",
            "-q:v",
            "8",
            "-f",
            "image2pipe",
            "-vcodec",
            "mjpeg",
            "pipe:1",
        ]);
        let bytes = output(&mut ffmpeg, MAX_OBSERVATION_IMAGE_BYTES).await?;
        let (width, height) = jpeg_dimensions(&bytes).ok_or_else(failure)?;
        let captured_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| failure())?
            .as_millis();
        ObservationFrame::new(
            u64::try_from(captured_at).map_err(|_| failure())?,
            width,
            height,
            ObservationImageFormat::Jpeg,
            &bytes,
        )
    }
}

async fn output(command: &mut Command, limit: usize) -> Result<Vec<u8>, AttachmentError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|_| failure())?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(failure)?
        .take(limit as u64 + 1);
    let mut bytes = Vec::new();
    // The outer attachment timeout drops this future and kills the process.
    stdout
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| failure())?;
    if bytes.len() > limit {
        return Err(failure());
    }
    if !child.wait().await.map_err(|_| failure())?.success() {
        return Err(failure());
    }
    Ok(bytes)
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    let mut offset = 2;
    while offset + 9 < bytes.len() {
        if bytes[offset] != 0xff {
            return None;
        }
        let length = usize::from(u16::from_be_bytes([bytes[offset + 2], bytes[offset + 3]]));
        if length < 2 {
            return None;
        }
        if matches!(bytes[offset + 1], 0xc0..=0xc2) {
            let height = u16::from_be_bytes([bytes[offset + 5], bytes[offset + 6]]);
            let width = u16::from_be_bytes([bytes[offset + 7], bytes[offset + 8]]);
            return Some((u32::from(width), u32::from(height)));
        }
        offset += length + 2;
    }
    None
}

fn failure() -> AttachmentError {
    AttachmentError::Transport("Screen capture unavailable".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires a real desktop display and FFmpeg"]
    async fn capture_live_desktop() {
        let source = ScreenObservation::desktop("Desktop").unwrap();
        let frame =
            tokio::time::timeout(super::super::OBSERVATION_TIMEOUT, source.capture("screen"))
                .await
                .unwrap()
                .unwrap();
        let value = serde_json::to_value(frame).unwrap();
        assert_eq!(value["mime_type"], "image/jpeg");
        assert!(value["width"].as_u64().unwrap() <= 960);
        assert!(value["height"].as_u64().unwrap() <= 960);
        assert!(!value["data"].as_str().unwrap().is_empty());
    }
}
