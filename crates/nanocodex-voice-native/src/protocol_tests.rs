use super::*;
use pretty_assertions::assert_eq;

#[test]
fn signaling_roundtrips_without_diagnostic_secrets() {
    let secret = "a=ice-pwd:synthetic-secret";
    let message = Message::ApplyAnswer {
        sdp: secret.to_owned().try_into().unwrap(),
    };
    let debug = format!("{message:?}");
    assert!(!debug.contains(secret));
    assert!(debug.contains("REDACTED"));
    assert_eq!(
        decode_frame(&encode_frame(&message).unwrap()).unwrap(),
        Some(message)
    );
}

#[test]
fn signaling_bounds_apply_to_untrusted_wire_input() {
    for sdp in [String::new(), "x".repeat(64 * 1024 + 1)] {
        let json = serde_json::json!({"type": "applyAnswer", "sdp": sdp}).to_string();
        let mut frame = (json.len() as u32).to_be_bytes().to_vec();
        frame.extend(json.as_bytes());
        let error = decode_frame(&frame).unwrap_err();
        assert_eq!(error.to_string(), "invalid voice frame");
    }
}

#[test]
fn pcm_chunks_are_bounded_on_both_sides_of_the_pipe() {
    for length in [0, MAX_PCM_SAMPLES + 1] {
        let message = Message::WritePcm {
            generation: 42,
            samples: vec![123; length],
        };
        assert!(encode_frame(&message).is_err());
        let payload = serde_json::to_vec(&message).unwrap();
        let mut wire = (payload.len() as u32).to_be_bytes().to_vec();
        wire.extend(payload);
        assert!(decode_frame(&wire).is_err());
    }
    let message = Message::WritePcm {
        generation: 42,
        samples: vec![i16::MIN; MAX_PCM_SAMPLES],
    };
    assert_eq!(
        decode_frame(&encode_frame(&message).unwrap()).unwrap(),
        Some(message)
    );
}
