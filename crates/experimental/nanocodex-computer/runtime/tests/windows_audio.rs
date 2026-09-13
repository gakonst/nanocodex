use serde_json::json;
use skyre::{Error, Result, platforms::windows_audio_model::*};
use std::{
    cell::Cell,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

fn format(rate: u32) -> Format {
    Format {
        rate,
        channels: 2,
        bits: 32,
        valid_bits: 32,
        block_align: 8,
        channel_mask: 3,
        encoding: Encoding::Float,
    }
}
fn packet(samples: &[[f32; 2]], position: u64) -> Packet {
    Packet {
        data: samples
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect(),
        frames: samples.len() as u32,
        flags: 0,
        position,
    }
}
fn tone(rate: u32, hz: f64, seconds: f64) -> Vec<[f32; 2]> {
    (0..(rate as f64 * seconds) as usize)
        .map(|i| {
            let value =
                (0.7 * (2. * std::f64::consts::PI * hz * i as f64 / rate as f64).sin()) as f32;
            [value, -value]
        })
        .collect()
}
fn convert(rate: u32, values: &[[f32; 2]], chunks: usize) -> Output {
    let mut c = Converter::new(format(rate), MAX_FRAMES).unwrap();
    for (i, values) in values.chunks(chunks).enumerate() {
        c.push(packet(values, (i * chunks) as u64)).unwrap();
    }
    c.finish().unwrap()
}

#[test]
fn wasapi_resamples_real_rates_with_packet_partition_invariance_and_stereo_phase() {
    for rate in [8_000, 24_000, 44_100, 48_000, 96_000, 192_000] {
        let samples = tone(rate, 440., 0.2);
        let one = convert(rate, &samples, samples.len());
        let many = convert(rate, &samples, 137);
        assert_eq!(
            one.samples, many.samples,
            "packet boundaries changed {rate}Hz conversion"
        );
        assert_eq!(one.samples.len(), 9_600);
        assert!(one.samples.chunks_exact(2).all(|v| v[0] == -v[1]));
        let left: Vec<_> = one.samples.chunks_exact(2).map(|v| v[0]).collect();
        let crossings = left[200..4600]
            .windows(2)
            .filter(|w| w[0] <= 0 && w[1] > 0)
            .count();
        let hz = crossings as f64 * RATE as f64 / 4400.;
        assert!(
            (hz - 440.).abs() < 8.,
            "relabeling changed pitch: {rate}→{hz}"
        );
    }
}

#[test]
fn wasapi_downsampling_filters_out_of_band_energy_instead_of_aliasing() {
    let rms = |hz| {
        let output = convert(48_000, &tone(48_000, hz, 0.2), 480);
        let interior = &output.samples[256..output.samples.len() - 256];
        (interior.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / interior.len() as f64).sqrt()
    };
    let audible = rms(1000.);
    let rejected = rms(18_000.);
    assert!(
        audible > 15_000.,
        "passband unexpectedly attenuated: {audible}"
    );
    assert!(
        rejected / audible < 0.01,
        "18kHz aliased into 24kHz output: {rejected}/{audible}"
    );
}

#[test]
fn wasapi_integer_pcm_valid_bits_float_clipping_and_channel_mask_downmix_are_real() {
    for (bits, valid_bits, bytes) in [
        (8, 8, vec![0, 128, 255]),
        (
            16,
            16,
            [-32768i16, 0, 32767]
                .into_iter()
                .flat_map(i16::to_le_bytes)
                .collect(),
        ),
        (24, 24, vec![0, 0, 128, 0, 0, 0, 255, 255, 127]),
        (
            32,
            24,
            [i32::MIN, 0, 0x7fffff00]
                .into_iter()
                .flat_map(i32::to_le_bytes)
                .collect(),
        ),
    ] {
        let f = Format {
            rate: RATE,
            channels: 1,
            bits,
            valid_bits,
            block_align: bits / 8,
            channel_mask: 4,
            encoding: Encoding::Pcm,
        };
        let mut c = Converter::new(f, 3).unwrap();
        c.push(Packet {
            data: bytes,
            frames: 3,
            flags: 0,
            position: 0,
        })
        .unwrap();
        let actual = c.finish().unwrap().samples;
        assert_eq!(&actual[..4], &[-32768, -32768, 0, 0]);
        assert_eq!(actual[4], if bits == 8 { 32512 } else { 32767 });
        assert_eq!(actual[4], actual[5]);
    }
    let mut float = Converter::new(format(RATE), 1).unwrap();
    float.push(packet(&[[2., -2.]], 0)).unwrap();
    assert_eq!(float.finish().unwrap().samples, [32767, -32768]);
    let mut wide = Converter::new(
        Format {
            bits: 64,
            valid_bits: 64,
            block_align: 16,
            ..format(RATE)
        },
        1,
    )
    .unwrap();
    wide.push(Packet {
        data: [0.25f64, -0.5]
            .into_iter()
            .flat_map(f64::to_le_bytes)
            .collect(),
        frames: 1,
        flags: 0,
        position: 0,
    })
    .unwrap();
    assert_eq!(wide.finish().unwrap().samples, [8192, -16384]);
    let f = Format {
        channels: 6,
        block_align: 24,
        channel_mask: 0x3f,
        ..format(RATE)
    };
    let mut c = Converter::new(f, 2).unwrap();
    c.push(Packet {
        data: [1f32; 12].into_iter().flat_map(f32::to_le_bytes).collect(),
        frames: 2,
        flags: 0,
        position: 0,
    })
    .unwrap();
    assert_eq!(
        c.finish().unwrap().samples,
        [32767; 4],
        "normalized multichannel DC must neither clip nor drop channels"
    );
    let mut c = Converter::new(f, 3).unwrap();
    c.push(Packet {
        data: [
            [1f32, 0., 0., 0., 0., 0.],
            [0f32, 0., 0., 0., 0., 1.],
            [0f32, 0., 1., 0., 0., 0.],
        ]
        .into_iter()
        .flatten()
        .flat_map(f32::to_le_bytes)
        .collect(),
        frames: 3,
        flags: 0,
        position: 0,
    })
    .unwrap();
    let mixed = c.finish().unwrap().samples;
    assert!(mixed[0] > 0 && mixed[1] == 0, "front-left leaked right");
    assert!(
        mixed[2] == 0 && mixed[3] > 0,
        "back-right lost or leaked left"
    );
    assert!(
        mixed[4] > 0 && mixed[4] == mixed[5],
        "center lost stereo symmetry"
    );
}

#[test]
fn wasapi_waveformat_records_validate_subformat_extents_rate_alignment_and_mask() {
    let mut wave = vec![];
    wave.extend(0xfffeu16.to_le_bytes());
    wave.extend(2u16.to_le_bytes());
    wave.extend(48_000u32.to_le_bytes());
    wave.extend(384_000u32.to_le_bytes());
    wave.extend(8u16.to_le_bytes());
    wave.extend(32u16.to_le_bytes());
    wave.extend(22u16.to_le_bytes());
    wave.extend(32u16.to_le_bytes());
    wave.extend(3u32.to_le_bytes());
    wave.extend([
        3, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71,
    ]);
    assert_eq!(parse_wave_format(&wave).unwrap(), format(48_000));
    for (offset, value) in [
        (0, 7),
        (2, 0),
        (8, 1),
        (12, 7),
        (14, 31),
        (16, 21),
        (18, 0),
        (20, 1),
        (24, 7),
        (28, 1),
    ] {
        let mut bad = wave.clone();
        bad[offset] = value;
        assert!(
            parse_wave_format(&bad).is_err(),
            "accepted malformed byte {offset}"
        );
    }
    assert!(parse_wave_format(&wave[..39]).is_err());
    for bad in [
        Format {
            rate: 0,
            ..format(RATE)
        },
        Format {
            channels: 3,
            block_align: 12,
            channel_mask: 0,
            ..format(RATE)
        },
        Format {
            channel_mask: 0x80000001,
            ..format(RATE)
        },
    ] {
        assert!(bad.validate().is_err());
    }
    assert!(format(RATE).packet_bytes(RATE + 1).is_err());
    assert!(wav(&[]).is_err());
    assert!(wav(&[1]).is_err());
}

#[test]
fn wasapi_silence_timestamp_errors_and_discontinuity_have_explicit_semantics() {
    let mut c = Converter::new(format(RATE), 10).unwrap();
    c.push(Packet {
        data: vec![],
        frames: 2,
        flags: SILENT | DISCONTINUITY,
        position: 42,
    })
    .unwrap();
    c.push(Packet {
        data: vec![],
        frames: 2,
        flags: SILENT | TIMESTAMP_ERROR,
        position: u64::MAX,
    })
    .unwrap();
    c.push(Packet {
        data: vec![],
        frames: 2,
        flags: SILENT,
        position: 500,
    })
    .unwrap();
    let out = c.finish().unwrap();
    assert_eq!(out.samples, [0; 12]);
    assert_eq!(out.timestamp_errors, 1);
    for (flags, position) in [(DISCONTINUITY, 1), (0, 3), (8, 1)] {
        let mut c = Converter::new(format(RATE), 10).unwrap();
        c.push(packet(&[[0., 0.]], 0)).unwrap();
        let mut p = packet(&[[0., 0.]], position);
        p.flags = flags;
        assert!(c.push(p).is_err());
    }
    for p in [
        packet(&[[f32::NAN, 0.]], 0),
        packet(&[[f32::INFINITY, 0.]], 0),
        Packet {
            data: vec![0; 7],
            frames: 1,
            flags: 0,
            position: 0,
        },
        Packet {
            data: vec![0; 8],
            frames: 1,
            flags: SILENT,
            position: 0,
        },
    ] {
        let mut c = Converter::new(format(RATE), 10).unwrap();
        assert!(c.push(p).is_err());
    }
    let mut c = Converter::new(
        Format {
            bits: 32,
            valid_bits: 24,
            encoding: Encoding::Pcm,
            ..format(RATE)
        },
        1,
    )
    .unwrap();
    assert!(
        c.push(Packet {
            data: [1i32, 0].into_iter().flat_map(i32::to_le_bytes).collect(),
            frames: 1,
            flags: 0,
            position: 0
        })
        .is_err()
    );
}

#[test]
fn wasapi_packet_lease_releases_once_on_success_error_and_unwind_and_preserves_release_errors() {
    let released = Cell::new(0);
    let result = PacketLease::new(7, |frames| {
        assert_eq!(frames, 7);
        released.set(released.get() + 1);
        Ok(())
    })
    .finish(Ok(17));
    assert_eq!(result.unwrap(), 17);
    assert_eq!(released.get(), 1);
    let error = PacketLease::new(3, |frames| {
        assert_eq!(frames, 0, "unread failed packet must remain unconsumed");
        released.set(released.get() + 1);
        Err(Error::action("release"))
    })
    .finish::<()>(Err(Error::invalid("copy")))
    .unwrap_err();
    assert_eq!(error.code, -32602);
    assert!(error.message.contains("copy"));
    assert!(error.message.contains("release"));
    assert_eq!(released.get(), 2);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _lease = PacketLease::new(1, |frames| {
            assert_eq!(
                frames, 0,
                "unwinding must not claim the packet was consumed"
            );
            released.set(released.get() + 1);
            Ok(())
        });
        panic!("owned conversion panic");
    }));
    assert!(result.is_err());
    assert_eq!(released.get(), 3);
    assert!(
        PacketLease::new(1, |_| Err(Error::action("release")))
            .finish(Ok(()))
            .is_err()
    );
}

#[test]
fn wasapi_frame_budget_and_wav_bytes_are_exact_at_fractional_source_rates() {
    let mut c = Converter::new(format(44_100), 2400).unwrap();
    c.push(packet(&tone(44_100, 1000., 0.2), 0)).unwrap();
    assert!(c.full());
    let output = c.finish().unwrap();
    assert_eq!(output.samples.len(), 4800);
    let wav = wav(&output.samples).unwrap();
    assert_eq!(wav.len(), 9644);
    assert_eq!(&wav[0..4], b"RIFF");
    assert_eq!(&wav[8..16], b"WAVEfmt ");
    assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 9636);
    assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), RATE);
    assert_eq!(u16::from_le_bytes(wav[22..24].try_into().unwrap()), 2);
    assert_eq!(u16::from_le_bytes(wav[34..36].try_into().unwrap()), 16);
    assert!(Converter::new(format(RATE), MAX_FRAMES + 1).is_err());
}

#[derive(Clone, Default)]
struct Log {
    calls: Arc<Mutex<Vec<(&'static str, thread::ThreadId)>>>,
}
impl Log {
    fn push(&self, name: &'static str) {
        self.calls
            .lock()
            .unwrap()
            .push((name, thread::current().id()));
    }
}
struct Fake {
    log: Log,
    packets: VecDeque<Packet>,
    delay: Duration,
    error: Option<Error>,
    running: bool,
    _not_send: Rc<()>,
}
use std::collections::VecDeque;
impl Provider for Fake {
    fn format(&self) -> Format {
        format(RATE)
    }
    fn start(&mut self, control: &Control) -> Result<()> {
        control.check_start()?;
        self.log.push("start");
        self.running = true;
        Ok(())
    }
    fn next_packet(&mut self) -> Result<Option<Packet>> {
        self.log.push("packet");
        if let Some(error) = self.error.take() {
            return Err(error);
        }
        Ok(self.packets.pop_front())
    }
    fn wait(&mut self, timeout: Duration) -> Result<()> {
        self.log.push("wait");
        thread::sleep(timeout.max(self.delay));
        Ok(())
    }
    fn stop(&mut self) -> Result<()> {
        if self.running {
            self.running = false;
            self.log.push("stop");
        }
        Ok(())
    }
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.log.push("drop");
    }
}
fn fake(log: &Log, packets: Vec<Packet>, delay: Duration) -> Box<dyn Provider> {
    log.push("create");
    Box::new(Fake {
        log: log.clone(),
        packets: packets.into(),
        delay,
        error: None,
        running: true,
        _not_send: Rc::new(()),
    })
}
fn wait_completed(audio: &mut Audio) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while audio.execute("status", "a", &json!({})).unwrap()["completed"] != true {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn wasapi_worker_keeps_non_send_provider_owned_and_checks_owner_before_consume_once() {
    let log = Log::default();
    let worker_log = log.clone();
    let mut audio = Audio::new(move |_| {
        Ok(fake(
            &worker_log,
            vec![packet(&vec![[0.25, -0.25]; 2400], 0)],
            Duration::ZERO,
        ))
    });
    audio
        .execute(
            "start",
            "a",
            &json!({"scope":"system","max_duration_ms":100}),
        )
        .unwrap();
    for method in ["start", "status", "stop"] {
        assert_eq!(
            audio.execute(method, "b", &json!({})).unwrap_err().code,
            -32001
        );
    }
    assert!(audio.end_session("b").is_err());
    wait_completed(&mut audio);
    assert!(audio.execute("start", "a", &json!({})).is_err());
    let result = audio.execute("stop", "a", &json!({})).unwrap();
    assert_eq!(result["frames"], 2400);
    assert_eq!(result["duration_ms"], 100);
    assert_eq!(result["sample_rate"], 24000);
    assert!(audio.execute("stop", "a", &json!({})).is_err());
    assert_eq!(
        audio.execute("status", "a", &json!({})).unwrap(),
        json!({"active":false})
    );
    let calls = log.calls.lock().unwrap();
    assert_eq!(calls.iter().filter(|(name, _)| *name == "stop").count(), 1);
    assert_eq!(calls.last().unwrap().0, "drop");
    assert!(
        calls
            .iter()
            .all(|(_, id)| *id == calls[0].1 && *id != thread::current().id())
    );
}

#[test]
fn wasapi_validation_never_opens_a_provider_for_bad_scope_owner_or_duration() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let mut audio = Audio::new(move |_| {
        seen.fetch_add(1, Ordering::SeqCst);
        Err(Error::action("opened"))
    });
    for args in [
        json!({"scope":"application"}),
        json!({"scope":null}),
        json!({"pid":1}),
        json!({"max_duration_ms":99}),
        json!({"max_duration_ms":300001}),
        json!({"max_duration_ms":100.5}),
        json!({"max_duration_ms":"100"}),
    ] {
        assert!(audio.execute("start", "a", &args).is_err());
    }
    assert!(audio.execute("start", "", &json!({})).is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(recording_duration(&json!({})).unwrap(), 60_000);
    assert_eq!(
        recording_duration(&json!({"max_duration_ms":null})).unwrap(),
        60_000
    );
    assert_eq!(
        recording_duration(&json!({"maxDurationMilliseconds":100})).unwrap(),
        100
    );
}

#[test]
fn wasapi_duration_finishes_without_fabricated_silence_and_owner_cancellation_discards_data() {
    let log = Log::default();
    let mut audio = Audio::new(move |_| {
        Ok(fake(
            &log,
            vec![packet(&vec![[0., 0.]; 240], 0)],
            Duration::ZERO,
        ))
    });
    let start = Instant::now();
    audio
        .execute("start", "a", &json!({"max_duration_ms":100}))
        .unwrap();
    wait_completed(&mut audio);
    assert!(start.elapsed() >= Duration::from_millis(90));
    assert_eq!(
        audio.execute("stop", "a", &json!({})).unwrap()["frames"],
        240
    );
    audio.execute("start", "a", &json!({})).unwrap();
    audio.end_session("a").unwrap();
    assert!(audio.execute("stop", "a", &json!({})).is_err());
    assert_eq!(
        audio.execute("status", "a", &json!({})).unwrap(),
        json!({"active":false})
    );
}

#[test]
fn wasapi_native_and_cleanup_errors_do_not_publish_audio_or_mask_failure_codes() {
    let log = Log::default();
    let mut audio = Audio::new(move |_| {
        Ok(Box::new(Fake {
            log: log.clone(),
            packets: VecDeque::new(),
            delay: Duration::ZERO,
            error: Some(Error::new(-12345, "owned device invalidation")),
            running: true,
            _not_send: Rc::new(()),
        }))
    });
    audio.execute("start", "a", &json!({})).unwrap();
    wait_completed(&mut audio);
    let error = audio.execute("stop", "a", &json!({})).unwrap_err();
    assert_eq!(error.code, -12345);
    assert!(audio.execute("stop", "a", &json!({})).is_err());
    struct BadStop;
    impl Provider for BadStop {
        fn format(&self) -> Format {
            format(RATE)
        }
        fn start(&mut self, control: &Control) -> Result<()> {
            control.check_start()
        }
        fn next_packet(&mut self) -> Result<Option<Packet>> {
            Ok(Some(packet(&vec![[0., 0.]; 2400], 0)))
        }
        fn wait(&mut self, _: Duration) -> Result<()> {
            Ok(())
        }
        fn stop(&mut self) -> Result<()> {
            Err(Error::new(-12346, "owned stop failure"))
        }
    }
    let mut audio = Audio::new(|_| Ok(Box::new(BadStop)));
    audio
        .execute("start", "a", &json!({"max_duration_ms":100}))
        .unwrap();
    wait_completed(&mut audio);
    assert_eq!(
        audio.execute("stop", "a", &json!({})).unwrap_err().code,
        -12346
    );
}

#[test]
fn wasapi_startup_and_stop_timeouts_bound_callers_cancel_late_work_and_disable_reuse() {
    let entered = Arc::new(AtomicUsize::new(0));
    let seen = entered.clone();
    let mut audio = Audio::with_timeout(
        move |control| {
            seen.fetch_add(1, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(100));
            control.check_start()?;
            panic!("late cancelled provider must not start");
        },
        Duration::from_millis(15),
    );
    assert_eq!(
        audio.execute("start", "a", &json!({})).unwrap_err().code,
        -32008
    );
    assert_eq!(
        audio.execute("start", "a", &json!({})).unwrap_err().code,
        -32008
    );
    thread::sleep(Duration::from_millis(110));
    assert_eq!(entered.load(Ordering::SeqCst), 1);
    let log = Log::default();
    let worker_log = log.clone();
    let mut audio = Audio::with_timeout(
        move |_| Ok(fake(&worker_log, vec![], Duration::from_millis(150))),
        Duration::from_millis(15),
    );
    audio.execute("start", "a", &json!({})).unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    while !log.calls.lock().unwrap().iter().any(|v| v.0 == "wait") {
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    let start = Instant::now();
    assert_eq!(
        audio.execute("stop", "a", &json!({})).unwrap_err().code,
        -32008
    );
    assert!(start.elapsed() < Duration::from_millis(100));
    assert_eq!(
        audio.execute("start", "a", &json!({})).unwrap_err().code,
        -32008
    );
    thread::sleep(Duration::from_millis(170));
    assert_eq!(log.calls.lock().unwrap().last().unwrap().0, "drop");
}

#[test]
fn wasapi_stop_drains_post_stop_packets_and_provider_returned_timeout_disables_reuse() {
    struct Drain {
        stopped: bool,
        emitted: bool,
    }
    impl Provider for Drain {
        fn format(&self) -> Format {
            format(RATE)
        }
        fn start(&mut self, control: &Control) -> Result<()> {
            control.check_start()
        }
        fn next_packet(&mut self) -> Result<Option<Packet>> {
            if self.stopped && !self.emitted {
                self.emitted = true;
                Ok(Some(packet(&[[0.5, -0.5]; 3], 7)))
            } else {
                Ok(None)
            }
        }
        fn wait(&mut self, timeout: Duration) -> Result<()> {
            thread::sleep(timeout);
            Ok(())
        }
        fn stop(&mut self) -> Result<()> {
            self.stopped = true;
            Ok(())
        }
    }
    let mut audio = Audio::new(|_| {
        Ok(Box::new(Drain {
            stopped: false,
            emitted: false,
        }))
    });
    audio.execute("start", "a", &json!({})).unwrap();
    assert_eq!(audio.execute("stop", "a", &json!({})).unwrap()["frames"], 3);
    let attempts = Arc::new(AtomicUsize::new(0));
    let seen = attempts.clone();
    let mut audio = Audio::new(move |_| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Fake {
            log: Log::default(),
            packets: VecDeque::new(),
            delay: Duration::ZERO,
            error: Some(Error::new(-32008, "owned provider timeout")),
            running: false,
            _not_send: Rc::new(()),
        }))
    });
    audio.execute("start", "a", &json!({})).unwrap();
    wait_completed(&mut audio);
    assert_eq!(
        audio.execute("stop", "a", &json!({})).unwrap_err().code,
        -32008
    );
    assert_eq!(
        audio.execute("start", "a", &json!({})).unwrap_err().code,
        -32008
    );
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[test]
fn wasapi_malformed_provider_format_is_rejected_and_cleaned_up_before_start() {
    struct BadFormat(Arc<AtomicUsize>);
    impl Provider for BadFormat {
        fn format(&self) -> Format {
            Format {
                rate: 0,
                ..format(RATE)
            }
        }
        fn start(&mut self, _: &Control) -> Result<()> {
            panic!("invalid format must never start capture")
        }
        fn next_packet(&mut self) -> Result<Option<Packet>> {
            panic!("invalid format must never read packets")
        }
        fn wait(&mut self, _: Duration) -> Result<()> {
            panic!("invalid format must never wait")
        }
        fn stop(&mut self) -> Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    let stopped = Arc::new(AtomicUsize::new(0));
    let seen = stopped.clone();
    let mut audio = Audio::new(move |_| Ok(Box::new(BadFormat(seen.clone()))));
    assert!(
        audio
            .execute("start", "a", &json!({}))
            .unwrap_err()
            .message
            .contains("format")
    );
    assert_eq!(stopped.load(Ordering::SeqCst), 1);
    assert_eq!(
        audio.execute("status", "a", &json!({})).unwrap(),
        json!({"active":false})
    );
}
