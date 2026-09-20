//! Linux PulseAudio / PipeWire-Pulse virtual input. Only session-owned modules
//! are unloaded; no global defaults or application routes are explicitly set.
//! PulseAudio may automatically select the virtual input on monitor-only hosts.
use super::SinkFactory;
#[cfg(any(target_os = "linux", test))]
use super::{AudioSink, Result};

pub async fn native_factory(machine_id: &str) -> Option<SinkFactory> {
    #[cfg(target_os = "linux")]
    {
        return match linux::factory(machine_id).await {
            Ok(factory) => Some(factory),
            Err(error) => {
                tracing::warn!(%error, "remote microphone unavailable");
                None
            }
        };
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = machine_id;
        None
    }
}

// Compiled in tests on macOS too, so Linux code cannot silently bitrot.
#[cfg(any(target_os = "linux", test))]
mod linux {
    use super::*;
    use std::{
        process::{Command, Stdio},
        sync::Arc,
        time::{Duration, Instant},
    };
    use tokio::io::AsyncWriteExt;

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
                    .take(65536)
                    .read_to_string(&mut text)?;
                if text.len() >= 65536 {
                    return Err("PulseAudio response exceeds limit".into());
                }
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
    pub(super) async fn factory(machine_id: &str) -> Result<SinkFactory> {
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
        .await??;
        if !player.success() {
            return Err("PulseAudio player unavailable".into());
        }
        let name = device_name(machine_id);
        // Cancellation drops the completed result. This creates only virtual
        // devices; PCM playback starts only when an authorized writer opens.
        let modules = tokio::task::spawn_blocking(move || create_modules(&name)).await??;
        Ok(factory_from_modules(Arc::new(modules)))
    }
    fn factory_from_modules(modules: Arc<Modules>) -> SinkFactory {
        Arc::new(move || Box::pin(open_modules(Arc::clone(&modules))))
    }
    fn device_name(machine_id: &str) -> String {
        format!(
            "nanocodex_remote_mic_{}",
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, machine_id.as_bytes()).simple()
        )
    }
    struct Modules {
        ids: Vec<String>,
        name: String,
    }
    impl Drop for Modules {
        fn drop(&mut self) {
            // The stable name must be released before publisher shutdown can
            // exit the process. Each command is bounded to two seconds.
            for id in std::mem::take(&mut self.ids).into_iter().rev() {
                if let Err(error) = pactl(&["unload-module", &id]) {
                    tracing::warn!(%error, "remote microphone module cleanup failed");
                }
            }
        }
    }
    fn module_id(value: String) -> Result<String> {
        value
            .parse::<u32>()
            .map_err(|_| "invalid PulseAudio module ID")?;
        Ok(value)
    }
    fn create_modules(name: &str) -> Result<Modules> {
        // A preexisting output prevents creating a null sink as the only/default
        // playback sink. Never request set-default-* or module-loopback.
        if pactl(&["get-default-sink"])?.is_empty() || pactl(&["get-default-source"])?.is_empty() {
            return Err("existing playback and input defaults required".into());
        }
        // PulseAudio prefers non-monitor sources over monitors. On a headless
        // host with only a monitor input, it may automatically select our source.
        // Leave that server policy intact; never reset a user's global defaults.
        let source = format!("{name}_source");
        // Never adopt or unload an existing device, including a racing publisher.
        for (kind, requested) in [("sinks", name), ("sources", source.as_str())] {
            if pactl(&["list", "short", kind])?
                .lines()
                .any(|line| line.split_whitespace().nth(1) == Some(requested))
            {
                return Err("remote microphone device name already occupied".into());
            }
        }
        let mut modules = Modules {
            ids: Vec::new(),
            name: name.into(),
        };
        let sink_name = format!("sink_name={name}");
        let mut sink_args = vec![
            "load-module",
            "module-null-sink",
            &sink_name,
            "rate=48000",
            "channels=1",
            "sink_properties='device.description=Nanocodex_Remote_Microphone_Input device.class=filter priority.session=0'",
        ];
        let server: serde_json::Value = serde_json::from_str(&pactl(&["--format=json", "info"])?)?;
        if server["server_name"]
            .as_str()
            .is_some_and(|name| name.eq_ignore_ascii_case("pulseaudio"))
        {
            // Pulse's default null-sink rewind window can buffer two seconds,
            // stalling the bounded PCM writer before a game opens its input.
            // norewinds bounds that window to 50 ms. PipeWire has a different
            // scheduler and does not document this Pulse-specific module option.
            sink_args.push("norewinds=1");
        }
        modules.ids.push(module_id(pactl(&sink_args)?)?);
        modules.ids.push(module_id(pactl(&["load-module", "module-remap-source", &format!("master={name}.monitor"), &format!("source_name={name}_source"), "source_properties='device.description=Nanocodex_Remote_Microphone device.class=filter priority.session=0'", "channels=1"])?)?);
        // Pulse may rename a device if another publisher wins the race. Verify
        // exact names AND returned module ownership; cleanup only our own IDs.
        for (kind, requested, owner) in [
            ("sinks", name, &modules.ids[0]),
            ("sources", source.as_str(), &modules.ids[1]),
        ] {
            let devices: serde_json::Value =
                serde_json::from_str(&pactl(&["--format=json", "list", kind])?)?;
            if !devices
                .as_array()
                .ok_or("invalid PulseAudio device list")?
                .iter()
                .any(|device| {
                    device["name"].as_str() == Some(requested)
                        && (device["owner_module"]
                            .as_u64()
                            .map(|id| id.to_string())
                            .or_else(|| device["owner_module"].as_str().map(str::to_owned)))
                        .as_deref()
                            == Some(owner.as_str())
                })
            {
                return Err("remote microphone device name or ownership mismatch".into());
            }
        }
        Ok(modules)
    }
    struct PulseSink {
        player: tokio::process::Child,
        input: Option<tokio::process::ChildStdin>,
        _modules: Arc<Modules>,
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
    async fn open_modules(modules: Arc<Modules>) -> Result<Box<dyn AudioSink>> {
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
                &modules.name,
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
    fn stable_device_name_is_machine_scoped() {
        assert_eq!(device_name("fixture"), device_name("fixture"));
        assert_ne!(device_name("fixture"), device_name("other"));
        assert_eq!(
            device_name("fixture"),
            format!(
                "nanocodex_remote_mic_{}",
                uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, b"fixture").simple()
            )
        );
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
        let machine_id = "synthetic-microphone-lifetime";
        let name = device_name(machine_id);
        let source_info: serde_json::Value =
            serde_json::from_str(&pactl(&["--format=json", "list", "sources"]).unwrap()).unwrap();
        let default_was_monitor = source_info.as_array().unwrap().iter().any(|source| {
            source["name"].as_str() == Some(default_source.as_str())
                && source["monitor_source"]
                    .as_str()
                    .is_some_and(|sink| !sink.is_empty())
        });
        let factory = factory(machine_id).await.unwrap();
        assert_eq!(pactl(&["get-default-sink"]).unwrap(), default_sink);
        let current_source = pactl(&["get-default-source"]).unwrap();
        assert!(
            current_source == default_source
                || (default_was_monitor && current_source == format!("{name}_source")),
            "only a monitor default may be automatically replaced by the owned virtual input"
        );
        assert!(
            self::factory(machine_id).await.is_err(),
            "occupied name must fail closed"
        );
        let source = format!("{name}_source");
        let mut sink = factory().await.unwrap();
        // A user can enable remote mic before the game opens its input. The
        // writer must remain live without a recorder driving monitor latency.
        for _ in 0..75 {
            tokio::time::timeout(Duration::from_millis(100), sink.write(&[0; 1920]))
                .await
                .expect("PCM must not stall before an input consumer opens")
                .unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
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
        // Muting closes the PCM writer but preserves the exact source identity.
        let sources = pactl(&["list", "short", "sources"]).unwrap();
        let before = sources
            .lines()
            .find(|line| line.split_whitespace().nth(1) == Some(source.as_str()))
            .unwrap()
            .to_owned();
        let reopened = factory().await.unwrap();
        assert!(
            pactl(&["list", "short", "sources"])
                .unwrap()
                .lines()
                .any(|line| {
                    line.split_whitespace()
                        .take(3)
                        .eq(before.split_whitespace().take(3))
                })
        );
        drop(factory);
        assert!(
            pactl(&["list", "short", "sources"])
                .unwrap()
                .contains(&source),
            "writer retains modules after factory drops"
        );
        drop(reopened);
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
