//! Linux PulseAudio / PipeWire-Pulse virtual input. Only session-owned modules
//! are unloaded; global defaults and existing application routing are untouched.
use super::{AudioSink, Result, SinkFactory};

pub async fn native_factory() -> Option<SinkFactory> {
    #[cfg(target_os = "linux")]
    {
        if linux::probe().await {
            return Some(std::sync::Arc::new(|| Box::pin(linux::open())));
        }
    }
    None
}

// Compiled in tests on macOS too, so Linux code cannot silently bitrot.
#[cfg(any(target_os = "linux", test))]
mod linux {
    use super::*;
    use std::{
        process::{Command, Stdio},
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, Instant},
    };
    use tokio::io::AsyncWriteExt;
    static NEXT: AtomicU64 = AtomicU64::new(0);

    // Every utility invocation is bounded, including module cleanup after drop.
    fn pactl(args: &[&str]) -> Result<String> {
        let mut child = Command::new("pactl")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let start = Instant::now();
        loop {
            if let Some(status) = child.try_wait()? {
                if !status.success() {
                    return Err("PulseAudio command failed".into());
                }
                use std::io::Read;
                let mut text = String::new();
                child
                    .stdout
                    .take()
                    .ok_or("missing PulseAudio response")?
                    .take(4096)
                    .read_to_string(&mut text)?;
                return Ok(text.trim().into());
            }
            if start.elapsed() >= Duration::from_secs(2) {
                let _ = child.kill();
                let _ = child.wait();
                return Err("PulseAudio command timed out".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    pub(super) async fn probe() -> bool {
        let player = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::process::Command::new("pacat")
                .arg("--version")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .status(),
        )
        .await
        .is_ok_and(|s| s.is_ok_and(|s| s.success()));
        if !player {
            return false;
        }
        // Advertise only after both the sink and remapped input actually exist.
        // This creates virtual devices only and never captures a physical mic.
        tokio::task::spawn_blocking(|| {
            create_modules().is_ok_and(|(mut modules, _)| modules.close().is_ok())
        })
        .await
        .unwrap_or(false)
    }
    struct Modules(Vec<String>);
    impl Modules {
        fn close(&mut self) -> Result<()> {
            let mut result = Ok(());
            for id in std::mem::take(&mut self.0).into_iter().rev() {
                if let Err(error) = pactl(&["unload-module", &id]) {
                    result = Err(error);
                }
            }
            result
        }
    }
    impl Drop for Modules {
        fn drop(&mut self) {
            let modules = std::mem::take(&mut self.0);
            if !modules.is_empty() {
                // Cleanup also works when the async runtime has shut down.
                let _ = std::thread::Builder::new()
                    .name("remote-mic-cleanup".into())
                    .spawn(move || {
                        for id in modules.into_iter().rev() {
                            if let Err(error) = pactl(&["unload-module", &id]) {
                                tracing::warn!(%error, "remote microphone module cleanup failed");
                            }
                        }
                    });
            }
        }
    }
    fn module_id(value: String) -> Result<String> {
        value
            .parse::<u32>()
            .map_err(|_| "invalid PulseAudio module ID")?;
        Ok(value)
    }
    fn create_modules() -> Result<(Modules, String)> {
        // A preexisting output prevents creating a null sink as the only/default
        // playback sink. Never request set-default-* or module-loopback.
        if pactl(&["get-default-sink"])?.is_empty() || pactl(&["get-default-source"])?.is_empty() {
            return Err("existing playback and input defaults required".into());
        }
        let name = format!(
            "nanocodex_remote_mic_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let mut modules = Modules(Vec::new());
        modules.0.push(module_id(pactl(&["load-module", "module-null-sink", &format!("sink_name={name}"), "rate=48000", "channels=1", "sink_properties='device.description=Nanocodex_Remote_Microphone_Input device.class=filter priority.session=0'"])?)?);
        modules.0.push(module_id(pactl(&["load-module", "module-remap-source", &format!("master={name}.monitor"), &format!("source_name={name}_source"), "source_properties='device.description=Nanocodex_Remote_Microphone device.class=filter priority.session=0'", "channels=1"])?)?);
        Ok((modules, name))
    }
    struct PulseSink {
        player: tokio::process::Child,
        input: Option<tokio::process::ChildStdin>,
        _modules: Modules,
    }
    impl Drop for PulseSink {
        fn drop(&mut self) {
            self.input.take();
            let _ = self.player.start_kill();
        }
    }
    #[async_trait::async_trait]
    impl AudioSink for PulseSink {
        async fn write(&mut self, pcm: &[u8]) -> Result<()> {
            self.input
                .as_mut()
                .ok_or("remote microphone closed")?
                .write_all(pcm)
                .await?;
            Ok(())
        }
    }
    pub(super) async fn open() -> Result<Box<dyn AudioSink>> {
        // A cancelled join still drops the completed result and unloads modules.
        let (modules, name) = tokio::task::spawn_blocking(create_modules).await??;
        open_modules(modules, name).await
    }
    async fn open_modules(modules: Modules, name: String) -> Result<Box<dyn AudioSink>> {
        let mut player = tokio::process::Command::new("pacat")
            .args([
                "--playback",
                "--raw",
                "--format=s16le",
                "--rate=48000",
                "--channels=1",
                "--latency-msec=20",
                "--client-name=Nanocodex Remote Microphone",
                "--device",
                &name,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let input = player.stdin.take().ok_or("missing microphone PCM pipe")?;
        Ok(Box::new(PulseSink {
            player,
            input: Some(input),
            _modules: modules,
        }))
    }
    #[test]
    fn module_ids_cannot_address_arbitrary_modules_or_arguments() {
        for id in ["", "1 2", "--help", "-1", "name", "4294967296"] {
            assert!(module_id(id.into()).is_err());
        }
        assert_eq!(module_id("42".into()).unwrap(), "42");
    }
    /// Run against an isolated PulseAudio server containing synthetic devices only.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "requires an isolated PulseAudio server"]
    async fn synthetic_pcm_reaches_virtual_source_and_drop_removes_devices() {
        use tokio::io::AsyncReadExt;
        let default_sink = pactl(&["get-default-sink"]).unwrap();
        let default_source = pactl(&["get-default-source"]).unwrap();
        assert!(probe().await);
        let (modules, name) = create_modules().unwrap();
        let source = format!("{name}_source");
        let mut sink = open_modules(modules, name.clone()).await.unwrap();
        let mut recorder = tokio::process::Command::new("parec")
            .args([
                "--raw",
                "--format=s16le",
                "--rate=48000",
                "--channels=1",
                "--latency-msec=20",
                "--device",
                &source,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut output = recorder.stdout.take().unwrap();
        let writing = tokio::spawn(async move {
            let pcm: Vec<u8> = (0..960)
                .flat_map(|i| {
                    let sample = ((i as f32 * 440.0 * std::f32::consts::TAU / 48000.0).sin()
                        * 8000.0) as i16;
                    sample.to_le_bytes()
                })
                .collect();
            for _ in 0..100 {
                sink.write(&pcm).await.unwrap();
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            sink
        });
        let peak = tokio::time::timeout(Duration::from_secs(5), async {
            let mut bytes = [0u8; 1920];
            loop {
                output.read_exact(&mut bytes).await.unwrap();
                let peak = bytes
                    .chunks_exact(2)
                    .map(|s| i16::from_le_bytes([s[0], s[1]]).unsigned_abs())
                    .max()
                    .unwrap();
                if peak > 1000 {
                    break peak;
                }
            }
        })
        .await
        .expect("synthetic tone must reach virtual input");
        assert!(peak > 1000);
        drop(writing.await.unwrap());
        recorder.kill().await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let sinks = pactl(&["list", "short", "sinks"]).unwrap();
                let sources = pactl(&["list", "short", "sources"]).unwrap();
                if !sinks.contains(&name) && !sources.contains(&source) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("drop must remove session-owned virtual devices");
        assert_eq!(pactl(&["get-default-sink"]).unwrap(), default_sink);
        assert_eq!(pactl(&["get-default-source"]).unwrap(), default_source);
    }
}
