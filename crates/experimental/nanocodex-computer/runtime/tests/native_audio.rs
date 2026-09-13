#![cfg(target_os = "macos")]
use serde_json::json;
use skyre::native::audio::{Audio, PcmFormat, RATE, convert_pcm, wav};
fn format() -> PcmFormat {
    PcmFormat {
        channels: 2,
        bits: 32,
        float: true,
        big_endian: false,
        planar: true,
        rate: RATE,
    }
}
#[test]
fn planar_float_clips_interleaves_and_caps_frame_count() {
    let a: Vec<_> = [-2f32, -0.5, 0.0, 0.5, 2.0]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect();
    let b: Vec<_> = [1f32, 0.5, 0.0, -0.5, -1.0]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect();
    assert_eq!(
        convert_pcm(&[(&a, 1), (&b, 1)], format(), 5).unwrap(),
        [
            -32768, 32767, -16384, 16384, 0, 0, 16384, -16384, 32767, -32768
        ]
    );
    assert_eq!(
        convert_pcm(&[(&a, 1), (&b, 1)], format(), 1).unwrap().len(),
        2
    );
    assert!(
        convert_pcm(&[(&a, 1), (&b, 1)], format(), 0)
            .unwrap()
            .is_empty()
    );
}
#[test]
fn integer_interleaving_and_mono_duplication_preserve_samples() {
    let bytes: Vec<_> = [-32768i16, 32767, -1, 1]
        .into_iter()
        .flat_map(i16::to_be_bytes)
        .collect();
    let f = PcmFormat {
        bits: 16,
        float: false,
        big_endian: true,
        planar: false,
        ..format()
    };
    assert_eq!(
        convert_pcm(&[(&bytes, 2)], f, 10).unwrap(),
        [-32768, 32767, -1, 1]
    );
    assert_eq!(
        convert_pcm(&[(&bytes, 1)], PcmFormat { channels: 1, ..f }, 10).unwrap(),
        [-32768, -32768, 32767, 32767, -1, -1, 1, 1]
    );
}
#[test]
fn pcm_rejects_malformed_layout_nonfinite_and_format_changes() {
    let a = 1f32.to_le_bytes();
    let b = f32::NAN.to_le_bytes();
    assert!(convert_pcm(&[(&a, 1)], format(), 1).is_err());
    assert!(convert_pcm(&[(&a, 1), (&b, 1)], format(), 1).is_err());
    assert!(convert_pcm(&[(&a[..3], 1), (&a, 1)], format(), 1).is_err());
    assert!(convert_pcm(&[(&a, 2), (&a, 1)], format(), 1).is_err());
    assert!(convert_pcm(&[(&a, 1), (&[], 1)], format(), 1).is_err());
    assert!(
        convert_pcm(
            &[(&a, 1), (&a, 1)],
            PcmFormat {
                rate: 48000,
                ..format()
            },
            1
        )
        .is_err()
    );
    assert!(
        convert_pcm(
            &[(&a, 1), (&a, 1)],
            PcmFormat {
                channels: 0,
                ..format()
            },
            1
        )
        .is_err()
    );
}
#[test]
fn wav_header_sizes_stereo_and_little_endian_samples() {
    let bytes = wav(&[-32768, 32767, 0, -1]).unwrap();
    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(&bytes[8..16], b"WAVEfmt ");
    assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 44);
    assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 24000);
    assert_eq!(u16::from_le_bytes(bytes[22..24].try_into().unwrap()), 2);
    assert_eq!(&bytes[36..40], b"data");
    assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 8);
    assert_eq!(&bytes[44..], [0, 128, 255, 127, 0, 0, 255, 255]);
    assert!(wav(&[1]).is_err());
}
#[test]
fn audio_validation_fails_before_any_capture_api() {
    let mut audio = Audio::default();
    assert_eq!(
        audio.execute("status", "owner", &json!({})).unwrap(),
        json!({"active":false})
    );
    for (owner, args) in [
        ("", json!({})),
        ("owner", json!({})),
        ("owner", json!({"pid":1,"scope":"system"})),
        ("owner", json!({"pid":1,"scope":17})),
        ("owner", json!({"pid":1,"scope":"invalid"})),
        ("owner", json!({"pid":1,"max_duration_ms":99})),
        ("owner", json!({"pid":1,"max_duration_ms":300001})),
        ("owner", json!({"pid":-1,"max_duration_ms":1000})),
        ("owner", json!({"pid":1,"max_duration_ms":"1000"})),
    ] {
        assert!(audio.execute("start", owner, &args).is_err());
    }
    assert!(audio.execute("stop", "owner", &json!({})).is_err());
    audio.end_session("owner").unwrap();
}

#[test]
fn installed_audio_duration_defaults_and_bounds_without_capture() {
    use skyre::native::audio::recording_duration;
    for args in [json!({}), json!({"max_duration_ms":null})] {
        assert_eq!(recording_duration(&args).unwrap(), 60_000);
    }
    for duration in [100, 60_001, 300_000] {
        assert_eq!(
            recording_duration(&json!({"max_duration_ms":duration})).unwrap(),
            duration
        );
    }
    for duration in [
        json!(99),
        json!(300001),
        json!(100.5),
        json!("100"),
        json!(-1),
    ] {
        assert!(recording_duration(&json!({"max_duration_ms":duration})).is_err());
    }
}
