//! Local, bounded microphone capture. Audio never leaves this module over a network.
use std::{
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tempfile::NamedTempFile;

pub(crate) const MAX_SECONDS: u64 = 120;
const MAX_BYTES: u64 = 20 * 1024 * 1024;

/// Owns a microphone subprocess and its private temporary output.
/// Dropping this value cancels capture, reaps the process, and removes the audio.
pub(crate) struct Recorder {
    child: Option<Child>,
    output: Option<NamedTempFile>,
    started_at: Instant,
    diagnostics: Arc<Mutex<Vec<u8>>>,
}

/// Keeps the validated WAV alive until its consumer has finished uploading it.
pub(crate) struct RecordedSample {
    output: NamedTempFile,
    duration: Duration,
}

impl RecordedSample {
    pub(crate) fn duration(&self) -> Duration {
        self.duration
    }

    pub(crate) fn path(&self) -> &Path {
        self.output.path()
    }

    /// Play locally. Dropping this future kills the player; no audio is uploaded.
    pub(crate) async fn play(&self) -> Result<(), String> {
        let mut command = if cfg!(target_os = "macos") {
            sanitized_program("/usr/bin/afplay")
        } else if cfg!(target_os = "linux") {
            let mut command = sanitized_program("ffplay");
            command.args([
                "-nodisp",
                "-autoexit",
                "-loglevel",
                "error",
                "-protocol_whitelist",
                "file,pipe",
            ]);
            command.env("SDL_AUDIODRIVER", "pulseaudio");
            command.env("PULSE_SERVER", local_pulse_server()?);
            command
        } else {
            return Err("Local recording playback is unavailable on this platform.".into());
        };
        command.arg(self.path());
        run_player(command).await
    }
}

impl Recorder {
    pub(crate) fn elapsed(&self) -> Duration {
        self.started_at
            .elapsed()
            .min(Duration::from_secs(MAX_SECONDS))
    }

    /// Read only the newest local PCM window for a visible microphone meter.
    pub(crate) fn peak(&self) -> u16 {
        let Some(output) = &self.output else {
            return 0;
        };
        let Ok(mut file) = std::fs::File::open(output.path()) else {
            return 0;
        };
        let Ok(metadata) = file.metadata() else {
            return 0;
        };
        let len = metadata.len();
        if len < 128 {
            return 0;
        }
        let start = len.saturating_sub(6400).max(128) & !1;
        if file.seek(SeekFrom::Start(start)).is_err() {
            return 0;
        }
        let mut bytes = [0_u8; 6400];
        let Ok(n) = file.read(&mut bytes) else {
            return 0;
        };
        pcm_peak(&bytes[..n])
    }

    /// The caller should stop/collect the sample when this returns true.
    pub(crate) fn is_finished(&mut self) -> Result<bool, String> {
        self.child
            .as_mut()
            .expect("capture child")
            .try_wait()
            .map(|status| status.is_some())
            .map_err(|e| format!("Cannot inspect microphone recorder: {e}"))
    }

    pub(crate) async fn start() -> Result<Self, String> {
        if cfg!(target_os = "macos") {
            let mut command = sanitized_program("nanocodex-voice-recorder");
            command.args(["--max-seconds", &MAX_SECONDS.to_string()]);
            Self::start_command(command, true).await
        } else {
            Self::start_input(microphone_input()?).await
        }
    }

    #[cfg(test)]
    pub(crate) async fn synthetic() -> Result<Self, String> {
        Self::start_input(
            [
                "-re",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=16000",
            ]
            .map(str::to_owned)
            .to_vec(),
        )
        .await
    }

    async fn start_input(input: Vec<String>) -> Result<Self, String> {
        let mut command = sanitized_command();
        command.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-protocol_whitelist",
            "file,pipe",
        ]);
        command.args(input);
        command.args([
            "-vn",
            "-ac",
            "1",
            "-ar",
            "16000",
            "-c:a",
            "pcm_s16le",
            "-t",
            &MAX_SECONDS.to_string(),
            "-fs",
            &MAX_BYTES.to_string(),
            "-flush_packets",
            "1",
            "-f",
            "wav",
        ]);
        Self::start_command(command, false).await
    }

    async fn start_command(mut command: Command, native: bool) -> Result<Self, String> {
        let output = tempfile::Builder::new()
            .prefix("nanocodex-voice-")
            .suffix(".wav")
            .tempfile()
            .map_err(|e| format!("Cannot create microphone recording: {e}"))?;
        command.arg(output.path());
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound && native {
                "The packaged microphone recorder is missing. Reinstall Nanocodex, or rebuild its native voice resources with scripts/build-voice-native.py.".to_owned()
            } else if e.kind() == std::io::ErrorKind::NotFound {
                "Recording needs ffmpeg. On macOS run `brew install ffmpeg`, then retry R. Homebrew installations are detected automatically.".to_owned()
            } else {
                format!("Cannot launch the microphone recorder: {e}")
            }
        })?;
        let diagnostics = Arc::new(Mutex::new(Vec::new()));
        let captured = diagnostics.clone();
        if let Some(mut stderr) = child.stderr.take() {
            // Drain continuously so a noisy device never blocks capture. Retain
            // only a bounded diagnostic tail; no credentials reach this child.
            std::thread::spawn(move || {
                let mut bytes = [0; 1024];
                while let Ok(n) = stderr.read(&mut bytes) {
                    if n == 0 {
                        break;
                    }
                    let mut tail = captured
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    tail.extend_from_slice(&bytes[..n]);
                    if tail.len() > 4096 {
                        let trim = tail.len() - 4096;
                        tail.drain(..trim);
                    }
                }
            });
        }
        let mut recorder = Self {
            child: Some(child),
            output: Some(output),
            started_at: Instant::now(),
            diagnostics,
        };
        // The native helper confirms that its engine has started. Keep the UI in
        // Starting until then, so opening the device does not consume recording time.
        if native {
            let opened = Instant::now();
            loop {
                if recorder.is_finished()? {
                    return Err(recorder.failure("Microphone capture could not start"));
                }
                if recorder
                    .diagnostics
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .windows(b"nanocodex-recorder ready\n".len())
                    .any(|bytes| bytes == b"nanocodex-recorder ready\n")
                {
                    recorder.started_at = Instant::now();
                    break;
                }
                // First use can display the macOS consent dialog. Keep the
                // cancellable Starting state while the user responds to it.
                let permission_pending = recorder
                    .diagnostics
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .windows(b"nanocodex-recorder permission\n".len())
                    .any(|bytes| bytes == b"nanocodex-recorder permission\n");
                let timeout = Duration::from_secs(if permission_pending { 120 } else { 10 });
                if opened.elapsed() >= timeout {
                    return Err(recorder.failure("Microphone capture did not become ready"));
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        } else {
            // Catch missing FFmpeg input backends/devices before reporting success.
            tokio::time::sleep(Duration::from_millis(300)).await;
            if recorder.is_finished()? {
                return Err(recorder.failure("Microphone capture could not start"));
            }
        }
        Ok(recorder)
    }

    fn failure(&self, context: &str) -> String {
        let bytes = self
            .diagnostics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let detail: String = String::from_utf8_lossy(&bytes)
            .chars()
            .filter(|c| !c.is_control() || *c == '\n')
            .take(1200)
            .collect();
        format!(
            "{context}. Check System Settings → Privacy & Security → Microphone and your default input device.\n{}",
            detail.trim()
        )
    }

    pub(crate) async fn stop(mut self) -> Result<RecordedSample, String> {
        let child = self.child.as_mut().expect("capture child");
        // Both capture backends finalize the WAV header on q; killing does not.
        if child
            .try_wait()
            .map_err(|e| format!("Cannot inspect microphone recorder: {e}"))?
            .is_none()
            && let Some(mut stdin) = child.stdin.take()
        {
            // A broken pipe can mean the duration/size limit already ended capture.
            let _ = stdin.write_all(b"q\n");
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            match child
                .try_wait()
                .map_err(|e| format!("Cannot stop microphone recorder: {e}"))?
            {
                Some(status) => break status,
                None if Instant::now() >= deadline => {
                    return Err(
                        "Microphone recorder did not stop in time; recording discarded.".into(),
                    );
                }
                None => tokio::time::sleep(Duration::from_millis(25)).await,
            }
        };
        if !status.success() {
            return Err(self.failure("Microphone recording failed"));
        }
        let mut output = self.output.take().expect("capture output");
        let frames = validate_wav(output.as_file_mut())
            .map_err(|e| format!("Invalid microphone recording: {e}"))?;
        Ok(RecordedSample {
            output,
            duration: Duration::from_secs_f64(frames as f64 / 16000.0),
        })
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // Reap before NamedTempFile is dropped, including on Windows where an
            // open subprocess handle could otherwise prevent file deletion.
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn microphone_input() -> Result<Vec<String>, String> {
    if cfg!(target_os = "linux") {
        // Pin PulseAudio to a local socket rather than honoring a user config
        // that could redirect the default server to another machine.
        Ok(vec![
            "-f".into(),
            "pulse".into(),
            "-server".into(),
            local_pulse_server()?,
            "-i".into(),
            "default".into(),
        ])
    } else {
        Err("Microphone recording is supported on macOS (native audio) and Linux (PulseAudio/PipeWire) with ffmpeg. On this platform, use /voice clone with an existing audio file.".into())
    }
}

fn local_pulse_server() -> Result<String, String> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or("Local audio requires XDG_RUNTIME_DIR and a local PulseAudio/PipeWire server.")?;
    Ok(format!("unix:{}", runtime.join("pulse/native").display()))
}

async fn run_player(command: Command) -> Result<(), String> {
    let mut command = tokio::process::Command::from(command);
    command
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|e| format!("Cannot start local playback: {e}. On Linux, install ffplay."))?;
    let status = tokio::time::timeout(Duration::from_secs(MAX_SECONDS + 5), child.wait())
        .await
        .map_err(|_| "Local playback timed out.".to_owned())?
        .map_err(|e| format!("Local playback failed: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("Local playback failed. Check the default audio output device.".into())
    }
}

fn sanitized_command() -> Command {
    sanitized_program("ffmpeg")
}

// GUI-launched terminals often omit Homebrew from PATH. Resolve the binary
// before clearing the child environment, including packaged and standard installs.
fn audio_program(program: &str) -> PathBuf {
    if Path::new(program).is_absolute() {
        return program.into();
    }
    let mut directories = Vec::new();
    if let Some(package) = std::env::var_os("NANOCODEX_VOICE_PACKAGE") {
        directories.push(PathBuf::from(package).join("nanocodex-resources/voice/bin"));
    }
    if let Ok(executable) = std::env::current_exe()
        && let Some(parent) = executable.parent()
    {
        directories.push(parent.join("nanocodex-resources/voice/bin"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        directories.extend(std::env::split_paths(&path));
    }
    if cfg!(target_os = "macos") {
        directories.extend([
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ]);
    }
    find_program(program, &directories).unwrap_or_else(|| program.into())
}

fn find_program(program: &str, directories: &[PathBuf]) -> Option<PathBuf> {
    directories
        .iter()
        .map(|directory| directory.join(program))
        .find(|path| {
            let Ok(metadata) = std::fs::metadata(path) else {
                return false;
            };
            if !metadata.is_file() {
                return false;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o111 == 0 {
                    return false;
                }
            }
            true
        })
}

pub(crate) fn sanitized_program(program: &str) -> Command {
    let mut command = Command::new(audio_program(program));
    command.env_clear();
    // Pass only runtime/device discovery settings, never API keys, account
    // credentials, proxy settings, FFREPORT, or dynamic loader overrides.
    for key in [
        "PATH",
        "HOME",
        "TMPDIR",
        "XDG_RUNTIME_DIR",
        "LANG",
        "LC_ALL",
        "SystemRoot",
        "WINDIR",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
}

fn pcm_peak(bytes: &[u8]) -> u16 {
    bytes
        .chunks_exact(2)
        .map(|sample| i16::from_le_bytes([sample[0], sample[1]]).unsigned_abs())
        .max()
        .unwrap_or(0)
}

fn validate_wav(file: &mut std::fs::File) -> Result<u64, String> {
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    if !(44..=MAX_BYTES).contains(&len) {
        return Err("empty or oversized WAV".into());
    }
    file.rewind().map_err(|e| e.to_string())?;
    let mut header = [0; 12];
    file.read_exact(&mut header).map_err(|e| e.to_string())?;
    if &header[..4] != b"RIFF" || &header[8..] != b"WAVE" {
        return Err("not a WAV file".into());
    }
    if u64::from(u32::from_le_bytes(header[4..8].try_into().unwrap())) + 8 != len {
        return Err("incomplete WAV header".into());
    }
    let mut pcm = false;
    let mut data = None;
    let mut offset = 12_u64;
    while offset + 8 <= len {
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| e.to_string())?;
        let mut chunk = [0; 8];
        file.read_exact(&mut chunk).map_err(|e| e.to_string())?;
        let size = u64::from(u32::from_le_bytes(chunk[4..].try_into().unwrap()));
        if offset + 8 + size > len {
            return Err("truncated WAV data".into());
        }
        if &chunk[..4] == b"fmt " && size >= 16 {
            let mut format = [0; 16];
            file.read_exact(&mut format).map_err(|e| e.to_string())?;
            pcm = format[..2] == 1_u16.to_le_bytes()
                && format[2..4] == 1_u16.to_le_bytes()
                && format[4..8] == 16000_u32.to_le_bytes()
                && format[14..16] == 16_u16.to_le_bytes();
        }
        if &chunk[..4] == b"data" {
            data = Some(size);
        }
        offset += 8 + size + (size % 2);
    }
    if !pcm || !data.is_some_and(|n| n > 0 && n.is_multiple_of(2) && n <= MAX_SECONDS * 16000 * 2) {
        return Err("expected nonempty mono 16 kHz PCM within the 120 second limit".into());
    }
    Ok(data.unwrap() / 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(samples: usize) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        let n = (samples * 2) as u32;
        file.write_all(b"RIFF").unwrap();
        file.write_all(&(36 + n).to_le_bytes()).unwrap();
        file.write_all(b"WAVEfmt \x10\0\0\0\x01\0\x01\0").unwrap();
        file.write_all(&16000_u32.to_le_bytes()).unwrap();
        file.write_all(&32000_u32.to_le_bytes()).unwrap();
        file.write_all(b"\x02\0\x10\0data").unwrap();
        file.write_all(&n.to_le_bytes()).unwrap();
        file.write_all(&vec![0; n as usize]).unwrap();
        file
    }

    #[test]
    fn microphone_meter_uses_actual_pcm_magnitude() {
        assert_eq!(pcm_peak(&[0, 0, 0, 0]), 0);
        assert_eq!(pcm_peak(&[0, 32, 0, 128]), 32768);
        assert_eq!(pcm_peak(&[0, 16, 0]), 4096);
    }

    #[test]
    fn validates_pcm_and_rejects_empty_truncated_or_excess_duration() {
        assert_eq!(validate_wav(wav(16000).as_file_mut()).unwrap(), 16000);
        assert!(validate_wav(wav(0).as_file_mut()).is_err());
        assert!(validate_wav(wav(16000 * 121).as_file_mut()).is_err());
        let mut truncated = wav(10);
        truncated.as_file().set_len(45).unwrap();
        assert!(validate_wav(truncated.as_file_mut()).is_err());
    }

    #[test]
    fn sample_and_cancelled_recorder_remove_audio() {
        let sample = RecordedSample {
            output: wav(10),
            duration: Duration::from_secs_f64(10.0 / 16000.0),
        };
        let path = sample.path().to_owned();
        drop(sample);
        assert!(!path.exists());
        let output = wav(10);
        let path = output.path().to_owned();
        drop(Recorder {
            child: None,
            output: Some(output),
            started_at: Instant::now(),
            diagnostics: Arc::default(),
        });
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn cancelling_reaps_child_and_removes_audio() {
        let child = Command::new("sleep").arg("60").spawn().unwrap();
        let pid = child.id();
        let output = wav(10);
        let path = output.path().to_owned();
        drop(Recorder {
            child: Some(child),
            output: Some(output),
            started_at: Instant::now(),
            diagnostics: Arc::default(),
        });
        assert!(!path.exists());
        assert!(
            !Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelling_playback_kills_player_without_playing_audio() {
        let pid_file = NamedTempFile::new().unwrap();
        let mut command = sanitized_program("/bin/sh");
        command.args(["-c", "echo $$ > \"$1\"; exec sleep 60", "test-player"]);
        command.arg(pid_file.path());
        let task = tokio::spawn(run_player(command));
        let deadline = Instant::now() + Duration::from_secs(5);
        let pid = loop {
            let content = std::fs::read_to_string(pid_file.path()).unwrap();
            if let Ok(pid) = content.trim().parse::<u32>() {
                break pid;
            }
            assert!(
                Instant::now() < deadline,
                "synthetic player failed to start"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        loop {
            let alive = Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success();
            if !alive {
                break;
            }
            assert!(Instant::now() < deadline, "cancelled player remains alive");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[cfg(unix)]
    #[test]
    fn resolves_packaged_or_homebrew_recorder_without_shell_path() {
        use std::os::unix::fs::PermissionsExt;
        let installed = tempfile::tempdir().unwrap();
        let binary = installed.path().join("ffmpeg");
        std::fs::write(&binary, b"test executable").unwrap();
        assert!(find_program("ffmpeg", &[installed.path().into()]).is_none());
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            find_program(
                "ffmpeg",
                &[installed.path().join("missing"), installed.path().into()]
            ),
            Some(binary)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_readiness_starts_clock_and_quit_collects_valid_wav() {
        // Exercise the child protocol without opening a real microphone. The
        // fixture is not complete until q, just like the native WAV header.
        let fixture = wav(16000);
        let mut command = sanitized_program("/bin/sh");
        command.args([
            "-c",
            "sleep 0.2; echo 'nanocodex-recorder ready' >&2; read action; [ \"$action\" = q ] && cp \"$1\" \"$2\"",
            "test-recorder",
        ]);
        command.arg(fixture.path());
        let began = Instant::now();
        let recorder = Recorder::start_command(command, true).await.unwrap();
        assert!(began.elapsed() >= Duration::from_millis(200));
        assert!(recorder.elapsed() < Duration::from_millis(150));
        let sample = recorder.stop().await.unwrap();
        assert_eq!(sample.duration(), Duration::from_secs(1));
        let path = sample.path().to_owned();
        drop(sample);
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_failure_before_ready_is_not_recording_success() {
        let mut command = sanitized_program("/bin/sh");
        command.args(["-c", "echo 'device unavailable' >&2; exit 3"]);
        let result = Recorder::start_command(command, true).await;
        assert!(matches!(result, Err(error) if error.contains("could not start")));
    }

    #[test]
    fn subprocess_environment_is_allowlisted() {
        let command = sanitized_command();
        assert!(command.get_envs().all(|(key, _)| {
            [
                "PATH",
                "HOME",
                "TMPDIR",
                "XDG_RUNTIME_DIR",
                "LANG",
                "LC_ALL",
                "SystemRoot",
                "WINDIR",
            ]
            .iter()
            .any(|allowed| key == *allowed)
        }));
    }
}
