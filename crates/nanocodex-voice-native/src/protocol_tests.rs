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
