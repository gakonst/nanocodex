use super::*;
use crate::devices::buffers::Buffers;
use crate::devices::buffers::Playback;
use crate::devices::playback::PlaybackPort;
use crate::incoming::Incoming;
use gst::glib::subclass::prelude::ObjectSubclassIsExt;
use gstreamer_audio::subclass::prelude::AudioSinkImpl;
use pretty_assertions::assert_eq;
use rtc::interceptor::Packet;
use rtc::interceptor::TaggedPacket;
use rtc::sansio::Protocol;
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn real_decoder_renders_current_rtp_and_rejects_pre_epoch_arrivals() {
    gst::init().unwrap();
    let root = std::env::var_os("NANOCODEX_TEST_VOICE_RUNTIME").map(std::path::PathBuf::from);
    // Ordinary Cargo builds use installed plugins; explicit fixtures must load exactly.
    if let Some(root) = root {
        for plugin in
            "app audioconvert audioresample coreelements opus rtp rtpmanager".split_whitespace()
        {
            let relative = if cfg!(target_os = "macos") {
                format!("plugins/libgst{plugin}.dylib")
            } else if cfg!(windows) {
                format!("bin/gst{plugin}.dll")
            } else {
                format!("lib/gstreamer-1.0/libgst{plugin}.so")
            };
            gst::Plugin::load_file(root.join(relative)).unwrap();
        }
    }
    let mut encoder = opus::Encoder::new(
        /*sample_rate*/ 48000,
        opus::Channels::Mono,
        opus::Application::Voip,
    )
    .unwrap();
    // Prepare media before the live clock starts so encoding cost cannot slow the sender.
    let packets: Vec<_> = (0..40u16)
        .map(|sequence| {
            let samples: Vec<_> = (0..960)
                .map(|n| ((usize::from(sequence) * 960 + n) as f32 * 0.07).sin() * 0.2)
                .collect();
            let mut encoded = vec![0; 4096];
            let len = encoder.encode_float(&samples, &mut encoded).unwrap();
            encoded.truncate(len);
            (sequence, encoded)
        })
        .collect();
    let old = Instant::now();
    let buffers = Arc::new(Buffers::new(
        /*input_rate*/ 48000, /*output_rate*/ 48000,
    ));
    buffers.set_speaker_disabled(/*disabled*/ false).unwrap();
    let port = PlaybackPort::new(buffers.clone(), /*rate*/ 48000);
    let playout = Playout::new(port.writer()).unwrap();
    let (mut incoming, mut ingress) = Incoming::new();
    incoming.set_suppressed(/*suppressed*/ false).unwrap();
    let audible = AtomicUsize::new(0);
    let last_callback_us = AtomicUsize::new(0);
    let max_callback_gap_us = AtomicUsize::new(0);
    let started = Instant::now();
    std::thread::scope(|scope| {
        // Device callbacks run independently of incoming RTP work.
        let (stop, stopping) = mpsc::channel::<Instant>();
        let callback_buffers = buffers.clone();
        let callback_audible = &audible;
        let last_callback_us = &last_callback_us;
        let max_callback_gap_us = &max_callback_gap_us;
        let consumer = scope.spawn(move || {
            let mut output = Playback::default();
            let mut deadline = None;
            loop {
                match stopping.try_recv() {
                    Ok(end) => deadline = Some(end),
                    Err(mpsc::TryRecvError::Disconnected) => break,
                    Err(mpsc::TryRecvError::Empty) => {}
                }
                if deadline.is_some_and(|end| Instant::now() >= end) {
                    break;
                }
                // This checks real decoding, not a wall-clock device cadence.
                // Drain available frames promptly so CI scheduling stalls do not
                // trigger the native sink's production backpressure deadline.
                if callback_buffers.queued.load(Ordering::Acquire) == 0 {
                    std::thread::park_timeout(Duration::from_millis(/*millis*/ 1));
                    continue;
                }
                let count = (0..960)
                    .filter_map(|_| output.next(&callback_buffers))
                    .filter(|sample| sample.abs() > 0.01)
                    .count();
                callback_audible.fetch_add(count, Ordering::Relaxed);
                let completed_us = started.elapsed().as_micros() as usize;
                let previous_us = last_callback_us.swap(completed_us, Ordering::Relaxed);
                max_callback_gap_us.fetch_max(completed_us - previous_us, Ordering::Relaxed);
            }
        });
        for (sequence, encoded) in packets {
            ingress
                .handle_read(TaggedPacket {
                    now: Instant::now(),
                    transport: Default::default(),
                    message: Packet::Rtp(rtc::rtp::Packet {
                        header: rtc::rtp::header::Header {
                            version: 2,
                            payload_type: crate::audio_track::OPUS_PAYLOAD_TYPE,
                            sequence_number: sequence,
                            timestamp: u32::from(sequence) * 960,
                            ssrc: 7,
                            ..Default::default()
                        },
                        payload: encoded.into(),
                    }),
                })
                .unwrap();
            let mut packet = incoming.take().unwrap().unwrap();
            // Exercise the decoder epoch boundary independently of ingress age limits.
            if sequence < 10 {
                packet.at = old;
            }
            playout.push(packet).unwrap();
            // RTP advances by 20 ms per packet; avoid accumulating work and wakeup delays.
            let next_packet =
                started + Duration::from_millis(/*millis*/ (u64::from(sequence) + 1) * 20);
            std::thread::sleep(next_packet.saturating_duration_since(Instant::now()));
            if sequence == 9 {
                assert_eq!(audible.load(Ordering::Relaxed), 0);
            }
            playout.check().unwrap();
            assert!(
                !buffers.failed.load(Ordering::Acquire),
                "packet {sequence}; elapsed={:?}, audible={}, queued={}, last_callback_us={}, max_callback_gap_us={}",
                started.elapsed(),
                audible.load(Ordering::Relaxed),
                buffers.queued.load(Ordering::Acquire),
                last_callback_us.load(Ordering::Relaxed),
                max_callback_gap_us.load(Ordering::Relaxed)
            );
        }
        let deadline = Instant::now() + Duration::from_secs(/*secs*/ 3);
        stop.send(deadline).unwrap();
        while audible.load(Ordering::Relaxed) <= 4800 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(/*millis*/ 20));
            playout.check().unwrap();
            assert!(
                !buffers.failed.load(Ordering::Acquire),
                "drain; elapsed={:?}, audible={}, queued={}, last_callback_us={}, max_callback_gap_us={}",
                started.elapsed(),
                audible.load(Ordering::Relaxed),
                buffers.queued.load(Ordering::Acquire),
                last_callback_us.load(Ordering::Relaxed),
                max_callback_gap_us.load(Ordering::Relaxed)
            );
            if Instant::now() >= deadline {
                break;
            }
        }
        drop(stop);
        consumer.join().unwrap();
        let audible = audible.load(Ordering::Relaxed);
        assert!(
            audible > 4800,
            "real decoded non-silent audio must reach the sink writer; observed {audible} samples"
        );
        buffers.set_speaker_disabled(/*disabled*/ true).unwrap();
        drop(playout);
        assert!(!buffers.failed.load(Ordering::Acquire));
        buffers.set_speaker_disabled(/*disabled*/ false).unwrap();
        let sink = Sink::new(port.writer());
        assert!(sink.imp().write(&f32::NAN.to_le_bytes()).is_err());
        assert!(buffers.failed.load(Ordering::Acquire));
    });
}

#[test]
fn continuous_speech_survives_device_cadence_and_packet_jitter() {
    gst::init().unwrap();
    let mut encoder =
        opus::Encoder::new(48_000, opus::Channels::Mono, opus::Application::Voip).unwrap();
    let packets: Vec<_> = (0..150u16)
        .map(|sequence| {
            let samples: Vec<_> = (0..960)
                .map(|n| ((usize::from(sequence) * 960 + n) as f32 * 0.07).sin() * 0.2)
                .collect();
            let mut encoded = vec![0; 4096];
            let len = encoder.encode_float(&samples, &mut encoded).unwrap();
            encoded.truncate(len);
            (sequence, encoded)
        })
        .collect();
    let buffers = Arc::new(Buffers::new(48_000, 48_000));
    buffers.set_speaker_disabled(false).unwrap();
    let port = PlaybackPort::new(buffers.clone(), 48_000);
    let playout = Playout::new(port.writer()).unwrap();
    let (mut incoming, mut ingress) = Incoming::new();
    incoming.set_suppressed(false).unwrap();
    let started = Instant::now();
    let recording = std::thread::scope(|scope| {
        let buffers = buffers.clone();
        let consumer = scope.spawn(move || {
            let mut output = Playback::default();
            let mut samples = Vec::new();
            for tick in 0..340 {
                let deadline = started + Duration::from_millis(tick * 10);
                std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
                samples.extend((0..480).map(|_| output.next(&buffers).unwrap_or(0.0)));
                buffers.last_dac_ns.store(
                    (buffers.clock.elapsed() + Duration::from_millis(10)).as_nanos() as u64,
                    Ordering::Release,
                );
            }
            samples
        });
        for (sequence, payload) in packets {
            // A 100 ms burst every five packets, as during network scheduling stalls.
            let jitter = if sequence % 5 == 0 { 100 } else { 0 };
            let deadline = started + Duration::from_millis(u64::from(sequence) * 20 + jitter);
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            ingress
                .handle_read(TaggedPacket {
                    now: Instant::now(),
                    transport: Default::default(),
                    message: Packet::Rtp(rtc::rtp::Packet {
                        header: rtc::rtp::header::Header {
                            version: 2,
                            payload_type: crate::audio_track::OPUS_PAYLOAD_TYPE,
                            sequence_number: sequence,
                            timestamp: u32::from(sequence) * 960,
                            ssrc: 7,
                            ..Default::default()
                        },
                        payload: payload.into(),
                    }),
                })
                .unwrap();
            playout.push(incoming.take().unwrap().unwrap()).unwrap();
        }
        consumer.join().unwrap()
    });
    let first = recording
        .iter()
        .position(|sample| sample.abs() > 0.01)
        .expect("audible start");
    let last = recording
        .iter()
        .rposition(|sample| sample.abs() > 0.01)
        .unwrap();
    let speech = &recording[first..=last];
    let audible = speech.iter().filter(|sample| sample.abs() > 0.01).count();
    let mut longest = 0;
    let mut gap = 0;
    for sample in speech {
        if sample.abs() < 0.001 {
            gap += 1;
            longest = longest.max(gap);
        } else {
            gap = 0;
        }
    }
    eprintln!(
        "continuous playout: {:.1}% audible, longest gap {:.1} ms, first audio {:.1} ms",
        audible as f64 * 100.0 / speech.len() as f64,
        longest as f64 / 48.0,
        first as f64 / 48.0
    );
    buffers.set_speaker_disabled(true).unwrap();
    drop(playout);
    assert!(
        audible as f64 / speech.len() as f64 > 0.90,
        "continuous tone must not be chopped"
    );
    assert!(
        longest <= 1440,
        "unexpected gap: {} ms",
        longest as f64 / 48.0
    );
}
