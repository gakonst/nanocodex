use super::*;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use futures::StreamExt;
use libwebrtc::{
    audio_frame::AudioFrame,
    audio_source::{AudioSourceOptions, native::NativeAudioSource},
    audio_stream::native::NativeAudioStream,
    media_stream_track::MediaStreamTrack,
    peer_connection::{AnswerOptions, IceGatheringState},
    peer_connection_factory::ContinualGatheringPolicy,
};

async fn pair() -> Result<(Media, PeerConnection, NativeAudioSource)> {
    let (media, offer) = Media::start().await?;
    let factory = PeerConnectionFactory::default();
    let mut config = RtcConfiguration::default();
    config.continual_gathering_policy = ContinualGatheringPolicy::GatherOnce;
    let remote = factory.create_peer_connection(config)?;
    let source = NativeAudioSource::new(AudioSourceOptions::default(), 48000, 1, 0);
    let track = factory.create_audio_track("test-tone", source.clone());
    remote.add_track(track.into(), &["test"])?;
    remote
        .set_remote_description(SessionDescription::parse(&offer, SdpType::Offer)?)
        .await?;
    let answer = remote.create_answer(AnswerOptions::default()).await?;
    remote.set_local_description(answer).await?;
    tokio::time::timeout(Duration::from_secs(15), async {
        while remote.ice_gathering_state() != IceGatheringState::Complete {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("gathering remote candidates")?;
    tokio::time::timeout(
        Duration::from_secs(15),
        media.answer(remote.current_local_description().unwrap().to_string()),
    )
    .await
    .context("connecting loopback peers")??;
    Ok((media, remote, source))
}

async fn pair_with_trickled_ice() -> Result<(Media, PeerConnection, NativeAudioSource)> {
    let (media, offer) = Media::start().await?;
    let (local_tx, mut local_rx) = tokio::sync::mpsc::unbounded_channel();
    media.peer.on_ice_candidate(Some(Box::new(move |candidate| {
        let _ = local_tx.send(candidate);
    })));
    let factory = PeerConnectionFactory::default();
    let mut config = RtcConfiguration::default();
    config.continual_gathering_policy = ContinualGatheringPolicy::GatherOnce;
    let remote = factory.create_peer_connection(config)?;
    let (remote_tx, mut remote_rx) = tokio::sync::mpsc::unbounded_channel();
    remote.on_ice_candidate(Some(Box::new(move |candidate| {
        let _ = remote_tx.send(candidate);
    })));
    let source = NativeAudioSource::new(AudioSourceOptions::default(), 48000, 1, 0);
    let track = factory.create_audio_track("test-tone", source.clone());
    remote.add_track(track.into(), &["test"])?;
    remote
        .set_remote_description(SessionDescription::parse(&offer, SdpType::Offer)?)
        .await?;
    let answer = remote.create_answer(AnswerOptions::default()).await?;
    remote.set_local_description(answer.clone()).await?;
    media.peer.set_remote_description(answer).await?;
    // Exchange candidates explicitly: this fixture has no managed signaling
    // server, and a current description is not available while an offer is pending.
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if media.peer.connection_state() == PeerConnectionState::Connected { break; }
            tokio::select! {
                Some(candidate) = local_rx.recv() => remote.add_ice_candidate(candidate).await?,
                Some(candidate) = remote_rx.recv() => media.peer.add_ice_candidate(candidate).await?,
                _ = tokio::time::sleep(Duration::from_millis(10)) => {},
            }
        }
        anyhow::Ok(())
    }).await.context("connecting loopback peers")??;
    Ok((media, remote, source))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opus_audio_is_continuous_through_real_peer_and_neteq() -> Result<()> {
    let (media, remote, source) = pair_with_trickled_ice().await?;
    let track = media
        .peer
        .receivers()
        .into_iter()
        .find_map(|r| match r.track() {
            Some(MediaStreamTrack::Audio(track)) => Some(track),
            _ => None,
        })
        .expect("negotiated audio");
    track.set_enabled(true);
    let mut stream = NativeAudioStream::new(track, 48000, 1);
    let producer = tokio::spawn(async move {
        let mut clock = tokio::time::interval(Duration::from_millis(10));
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        for frame in 0..400 {
            clock.tick().await;
            let mut audio = AudioFrame::new(48000, 1, 480);
            for (index, sample) in audio.data.to_mut().iter_mut().enumerate() {
                *sample = (((frame * 480 + index) as f64 * 440.0 * std::f64::consts::TAU / 48000.0)
                    .sin()
                    * 12000.0) as i16;
            }
            source.capture_frame(&audio).await.unwrap();
        }
    });
    let mut frames = 0;
    let mut silent = 0;
    let mut longest_gap = 0;
    let mut gap = 0;
    tokio::time::timeout(Duration::from_secs(8), async {
        while frames < 350 {
            let frame = stream.next().await.expect("audio stream ended");
            frames += 1;
            if frames <= 30 {
                continue;
            } // Opus/NetEq warmup.
            if frame.data.iter().all(|sample| sample.unsigned_abs() < 100) {
                silent += 1;
                gap += 1;
                longest_gap = longest_gap.max(gap);
            } else {
                gap = 0;
            }
        }
    })
    .await
    .with_context(|| format!("receiving decoded audio: {frames} frames"))?;
    producer.abort();
    let stats = media.peer.get_stats().await?;
    let inbound = stats
        .into_iter()
        .find_map(|s| match s {
            RtcStats::InboundRtp(s) => Some(s.inbound),
            _ => None,
        })
        .unwrap();
    eprintln!(
        "frames={frames} silent={silent} longest_gap_ms={} received_samples={} concealed_samples={}",
        longest_gap * 10,
        inbound.total_samples_received,
        inbound.concealed_samples
    );
    ensure!(silent < 16 && longest_gap <= 3, "audio discontinuity");
    ensure!(
        inbound.total_samples_received > 48000 * 2,
        "missing decoded samples"
    );
    stream.close();
    remote.close();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "opens the local audio devices; run explicitly on the development machine"]
async fn platform_devices_start_muted_and_resume_after_unmute() -> Result<()> {
    let (mut media, remote, _source) = pair().await?;
    media.open_devices()?;
    media.controls(AudioControls {
        microphone_muted: true,
        speaker_suppressed: false,
    })?;
    ensure!(
        media.factory.adm_playout_enabled() && media.factory.playout_is_initialized(),
        "speaker device did not start"
    );
    for muted in [true, false, true, false, true] {
        media.controls(AudioControls {
            microphone_muted: muted,
            speaker_suppressed: true,
        })?;
        ensure!(media.microphone.enabled() != muted, "track mute state");
        ensure!(
            media.factory.adm_recording_enabled() != muted,
            "device mute state"
        );
        if !muted {
            ensure!(
                media.factory.recording_is_initialized(),
                "capture did not restart"
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    remote.close();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires BlackHole; records only the explicit virtual test device"]
async fn platform_playout_reaches_device_without_gaps() -> Result<()> {
    let (mut media, remote, source) = pair().await?;
    media.open_devices()?;
    for index in 0..media.factory.playout_devices() as u16 {
        eprintln!(
            "output {index}: {}",
            media.factory.playout_device_name(index)
        );
    }
    for index in 0..media.factory.recording_devices() as u16 {
        eprintln!(
            "input {index}: {}",
            media.factory.recording_device_name(index)
        );
    }
    let output = (0..media.factory.playout_devices() as u16)
        .find(|i| media.factory.playout_device_name(*i).contains("BlackHole"))
        .expect("BlackHole output");
    ensure!(
        media.factory.set_playout_device(output),
        "select virtual output"
    );
    let device = cpal::default_host()
        .input_devices()?
        .find(|d| {
            d.description()
                .is_ok_and(|d| d.name().contains("BlackHole"))
        })
        .expect("BlackHole input");
    let supported = device.default_input_config()?;
    ensure!(
        supported.sample_format() == cpal::SampleFormat::F32,
        "float input"
    );
    let config: cpal::StreamConfig = supported.into();
    let samples = Arc::new(std::sync::Mutex::new(Vec::<f32>::new()));
    let captured = samples.clone();
    let stream = device.build_input_stream(
        config,
        move |data: &[f32], _| {
            captured.lock().unwrap().extend_from_slice(data);
        },
        |error| eprintln!("capture error: {error}"),
        None,
    )?;
    stream.play()?;
    media.controls(AudioControls {
        microphone_muted: false,
        speaker_suppressed: false,
    })?;
    let mut clock = tokio::time::interval(Duration::from_millis(10));
    clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    for frame in 0..800 {
        clock.tick().await;
        if frame == 500 || frame == 600 {
            media.controls(AudioControls {
                microphone_muted: false,
                speaker_suppressed: frame == 500,
            })?;
        }
        let mut audio = AudioFrame::new(48000, 1, 480);
        for (index, sample) in audio.data.to_mut().iter_mut().enumerate() {
            *sample = (((frame * 480 + index) as f64 * 440.0 * std::f64::consts::TAU / 48000.0)
                .sin()
                * 12000.0) as i16;
        }
        source.capture_frame(&audio).await?;
    }
    stream.pause()?;
    drop(stream);
    let samples = samples.lock().unwrap();
    let block = config.sample_rate as usize / 100 * config.channels as usize;
    let levels: Vec<_> = samples
        .chunks_exact(block)
        .skip(100)
        .take(350)
        .map(|samples| {
            (samples.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
        })
        .collect();
    let silent = levels.iter().filter(|rms| **rms < 0.005).count();
    eprintln!(
        "device_rate={} channels={} blocks={} silent={} rms_min={:?} rms_max={:?}",
        config.sample_rate,
        config.channels,
        levels.len(),
        silent,
        levels.iter().copied().reduce(f64::min),
        levels.iter().copied().reduce(f64::max)
    );
    if let Ok(path) = std::env::var("NANOCODEX_AUDIO_CAPTURE") {
        std::fs::write(
            path,
            samples
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>(),
        )?;
    }
    let rms = |start: usize, count: usize| {
        samples
            .chunks_exact(block)
            .skip(start)
            .take(count)
            .map(|samples| {
                (samples.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / samples.len() as f64)
                    .sqrt()
            })
            .collect::<Vec<_>>()
    };
    let suppressed = rms(540, 40);
    let resumed = rms(640, 100);
    ensure!(
        suppressed.len() == 40 && suppressed.iter().all(|v| *v < 0.0001),
        "suppressed track leaked to speaker"
    );
    ensure!(
        resumed.len() == 100 && resumed.iter().all(|v| *v > 0.005),
        "speaker did not resume continuously"
    );
    remote.close();
    ensure!(levels.len() == 350 && silent <= 3, "device playback gaps");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires BlackHole; sends only a generated tone through the virtual microphone"]
async fn speaker_suppression_does_not_interrupt_microphone_capture() -> Result<()> {
    let (mut media, remote, _source) = pair().await?;
    media.open_devices()?;
    let input = (0..media.factory.recording_devices() as u16)
        .find(|i| {
            media
                .factory
                .recording_device_name(*i)
                .contains("BlackHole")
        })
        .expect("BlackHole input");
    ensure!(
        media.factory.set_recording_device(input),
        "select virtual microphone"
    );
    let device = cpal::default_host()
        .output_devices()?
        .find(|d| {
            d.description()
                .is_ok_and(|d| d.name().contains("BlackHole"))
        })
        .expect("BlackHole output");
    let supported = device.default_output_config()?;
    ensure!(
        supported.sample_format() == cpal::SampleFormat::F32,
        "float output"
    );
    let config: cpal::StreamConfig = supported.into();
    let mut index = 0u64;
    let output = device.build_output_stream(
        config,
        move |data: &mut [f32], _| {
            for frame in data.chunks_mut(config.channels as usize) {
                let sample = (index as f64 * 440.0 * std::f64::consts::TAU
                    / config.sample_rate as f64)
                    .sin() as f32
                    * 0.2;
                frame.fill(sample);
                index += 1;
            }
        },
        |error| eprintln!("tone output error: {error}"),
        None,
    )?;
    let track = remote
        .receivers()
        .into_iter()
        .find_map(|r| match r.track() {
            Some(MediaStreamTrack::Audio(track)) => Some(track),
            _ => None,
        })
        .expect("remote microphone track");
    let mut stream = NativeAudioStream::new(track, 48000, 1);
    output.play()?;
    media.controls(AudioControls {
        microphone_muted: false,
        speaker_suppressed: true,
    })?;
    let mut levels = Vec::new();
    tokio::time::timeout(Duration::from_secs(8), async {
        for frame_index in 0..500 {
            if frame_index % 10 == 0 {
                media
                    .controls(AudioControls {
                        microphone_muted: false,
                        speaker_suppressed: frame_index % 20 == 0,
                    })
                    .unwrap();
            }
            let frame = stream.next().await.expect("microphone stream ended");
            if frame_index >= 100 {
                levels.push(
                    (frame
                        .data
                        .iter()
                        .map(|v| (*v as f64 / 32768.0).powi(2))
                        .sum::<f64>()
                        / frame.data.len() as f64)
                        .sqrt(),
                );
            }
        }
    })
    .await?;
    output.pause()?;
    let silent = levels.iter().filter(|rms| **rms < 0.001).count();
    eprintln!(
        "capture blocks={} silent={} rms_min={:?} rms_max={:?}",
        levels.len(),
        silent,
        levels.iter().copied().reduce(f64::min),
        levels.iter().copied().reduce(f64::max)
    );
    stream.close();
    remote.close();
    ensure!(
        levels.len() == 400 && silent <= 3,
        "microphone capture gaps"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires BlackHole; uses only explicit virtual capture/playout and local peer"]
async fn external_pcm_uses_factory_device_while_provider_suppressed_and_mic_active() -> Result<()> {
    use codex_realtime_webrtc::PcmStatus;
    let (mut media, remote, source) = pair_with_trickled_ice().await?;
    media.open_devices()?;
    let output = (0..media.factory.playout_devices() as u16)
        .find(|i| media.factory.playout_device_name(*i).contains("BlackHole"))
        .context("BlackHole output")?;
    let input = (0..media.factory.recording_devices() as u16)
        .find(|i| {
            media
                .factory
                .recording_device_name(*i)
                .contains("BlackHole")
        })
        .context("BlackHole input")?;
    ensure!(
        media.factory.set_playout_device(output),
        "select virtual speaker"
    );
    ensure!(
        media.factory.set_recording_device(input),
        "select virtual microphone"
    );
    let device = cpal::default_host()
        .input_devices()?
        .find(|d| {
            d.description()
                .is_ok_and(|d| d.name().contains("BlackHole"))
        })
        .context("BlackHole monitor")?;
    let supported = device.default_input_config()?;
    ensure!(
        supported.sample_format() == cpal::SampleFormat::F32,
        "float monitor"
    );
    let config: cpal::StreamConfig = supported.into();
    let samples = Arc::new(std::sync::Mutex::new(Vec::<f32>::with_capacity(1_000_000)));
    let captured = samples.clone();
    let stream = device.build_input_stream(
        config,
        move |data: &[f32], _| {
            let mut captured = captured.lock().unwrap();
            if captured.len() + data.len() <= 1_000_000 {
                captured.extend_from_slice(data);
            }
        },
        |_| {},
        None,
    )?;
    stream.play()?;
    media.controls(AudioControls {
        microphone_muted: false,
        speaker_suppressed: true,
    })?;
    ensure!(
        media.microphone.enabled() && media.factory.adm_recording_enabled(),
        "microphone inactive"
    );
    ensure!(media.pcm.begin(123, 24000) == PcmStatus::Ready, "begin PCM");
    let mut max_peak = 0;
    let mut clock = tokio::time::interval(Duration::from_millis(10));
    clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut external_sample = 0usize;
    for frame in 0..400 {
        clock.tick().await;
        // Provider keeps producing throughout. The output must be silent except
        // while external PCM is admitted (frames 50..350).
        let mut provider = AudioFrame::new(48000, 1, 480);
        for (i, sample) in provider.data.to_mut().iter_mut().enumerate() {
            *sample = (((frame * 480 + i) as f64 * 880.0 * std::f64::consts::TAU / 48000.0).sin()
                * 10000.0) as i16;
        }
        source.capture_frame(&provider).await?;
        // HTTP-like 40ms bursts with alternating 80/10/60/10ms stalls.
        // Keep the waveform phase continuous independently of arrival time.
        if (50..350).contains(&frame) && [0, 8, 9, 15].contains(&((frame - 50) % 16)) {
            let external: Vec<_> = (0..960)
                .map(|i| {
                    (((external_sample + i) as f64 * 440.0 * std::f64::consts::TAU / 24000.0).sin()
                        * 10000.0) as i16
                })
                .collect();
            external_sample += external.len();
            ensure!(
                media.pcm.write(123, &external) == PcmStatus::Ready,
                "PCM backpressure while paced"
            );
        }
        if frame == 350 {
            ensure!(media.pcm.cancel(123) == PcmStatus::Ready, "cancel PCM");
            ensure!(
                media.pcm.write(123, &[10000; 240]) == PcmStatus::Stale,
                "cancelled generation accepted samples"
            );
            ensure!(
                media.pcm.begin(123, 24000) == PcmStatus::Stale,
                "cancelled generation restarted"
            );
            ensure!(
                media.pcm.begin(124, 24000) == PcmStatus::Ready,
                "fresh generation rejected"
            );
            ensure!(
                media.pcm.cancel(123) == PcmStatus::Stale,
                "stale cancellation touched fresh generation"
            );
            ensure!(
                media.pcm.cancel(124) == PcmStatus::Ready,
                "fresh generation was invalidated"
            );
        }
        max_peak = max_peak.max(media.pcm.take_peak());
    }
    stream.pause()?;
    drop(stream);
    ensure!(
        max_peak > 9000,
        "PCM did not reach production factory mixer"
    );
    ensure!(
        media.microphone.enabled() && media.factory.adm_recording_enabled(),
        "PCM playback or cancellation muted capture"
    );
    ensure!(
        media
            .peer
            .receivers()
            .iter()
            .filter_map(|receiver| receiver.track())
            .all(|track| !track.enabled()),
        "provider receiver became enabled during PCM playback"
    );
    let outbound_received = remote
        .get_stats()
        .await?
        .into_iter()
        .filter_map(|s| match s {
            RtcStats::InboundRtp(s) => Some(s.inbound.total_samples_received),
            _ => None,
        })
        .sum::<u64>();
    ensure!(
        outbound_received > 48000 * 2,
        "microphone capture did not continue"
    );
    let samples = samples.lock().unwrap();
    let block = config.sample_rate as usize / 100 * config.channels as usize;
    let levels: Vec<_> = samples
        .chunks_exact(block)
        .map(|chunk| {
            (chunk.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / chunk.len() as f64).sqrt()
        })
        .collect();
    ensure!(levels.len() >= 390, "insufficient monitor samples");
    ensure!(
        levels[20..40].iter().all(|v| *v < 0.0001),
        "provider audio leaked before PCM"
    );
    let silent = levels[100..330].iter().filter(|v| **v < 0.005).count();
    ensure!(silent <= 3, "external PCM output gaps: {silent}");
    ensure!(
        levels[375..390].iter().all(|v| *v < 0.0001),
        "audio continued after cancel"
    );
    eprintln!(
        "jittered external PCM BlackHole: audible_blocks={} silent_blocks={} peak={} mic_samples={}",
        230 - silent,
        silent,
        max_peak,
        outbound_received
    );
    remote.close();
    Ok(())
}
