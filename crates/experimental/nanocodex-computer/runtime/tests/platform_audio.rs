#![cfg(unix)]
use serde_json::json;
use skyre::platforms::Platforms;
use std::{fs, os::unix::fs::PermissionsExt};
fn executable(root: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = root.join(name);
    fs::write(&path, format!("#!/usr/bin/python3\n{body}")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}
#[test]
fn platform_linux_audio_progress_start_owned_stop_and_cleanup() {
    let temp = tempfile::tempdir().unwrap();
    let pactl = executable(temp.path(), "pactl", "print('fixture-sink')\n");
    let ffmpeg = executable(
        temp.path(),
        "ffmpeg",
        "import sys,wave,pathlib\npath=pathlib.Path(sys.argv[-1])\nwith wave.open(str(path),'wb') as f:\n f.setnchannels(2);f.setsampwidth(2);f.setframerate(24000);f.writeframes(bytes(96))\nprint('progress=continue',file=sys.stderr,flush=True)\nsys.stdin.readline()\n",
    );
    let mut platforms = Platforms::new();
    platforms.configure(&json!({"id":"audio","kind":"linux_audio","pactl":pactl,"ffmpeg":ffmpeg,"directory":temp.path().join("audio")})).unwrap();
    let call = |method: &str, params: serde_json::Value| json!({"id":"audio","method":method,"params":params});
    assert!(
        platforms
            .execute("platform.call", &call("stop_audio_recording", json!({})))
            .is_err()
    );
    assert!(
        platforms
            .execute(
                "platform.call",
                &call("start_audio_recording", json!({"max_duration_ms":99}))
            )
            .is_err()
    );
    platforms
        .execute(
            "platform.call",
            &call("start_audio_recording", json!({"max_duration_ms":1000})),
        )
        .unwrap();
    assert!(
        platforms
            .execute("platform.call", &call("start_audio_recording", json!({})))
            .is_err()
    );
    let result = platforms
        .execute("platform.call", &call("stop_audio_recording", json!({})))
        .unwrap();
    let path = result["filepath"].as_str().unwrap();
    assert!(std::path::Path::new(path).exists());
    assert_eq!(result["mime_type"], "audio/wav");
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        platforms
            .execute("platform.call", &call("clear", json!({})))
            .unwrap()["removed"],
        1
    );
    assert!(!std::path::Path::new(path).exists());
}
#[test]
fn platform_registration_is_host_only() {
    let mut platforms = Platforms::new();
    assert_eq!(
        platforms
            .execute(
                "platform.register",
                &json!({"id":"bad","kind":"linux_helper","executable":"/bin/sh"})
            )
            .unwrap_err()
            .code,
        -32003
    );
}

#[test]
fn configured_audio_kernel_cancellation_discards_partial_output_and_allows_restart() {
    let temp = tempfile::tempdir().unwrap();
    let pactl = executable(temp.path(), "pactl", "print('fixture-sink')\n");
    let ffmpeg = executable(
        temp.path(),
        "ffmpeg",
        "import sys,pathlib\npathlib.Path(sys.argv[-1]).write_bytes(b'partial-owned-recording')\nprint('progress=continue',file=sys.stderr,flush=True)\nsys.stdin.readline()\n",
    );
    let directory = temp.path().join("audio");
    let mut platforms = Platforms::new();
    platforms.configure(&json!({"id":"audio","kind":"linux_audio","pactl":pactl,"ffmpeg":ffmpeg,"directory":directory})).unwrap();
    for _ in 0..2 {
        platforms
            .sky_audio("start_audio_recording", &json!({"max_duration_ms":1000}))
            .unwrap();
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        platforms.cancel_sky_audio().unwrap();
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
        let status = platforms
            .execute(
                "platform.call",
                &json!({"id":"audio","method":"status","params":{}}),
            )
            .unwrap();
        assert_eq!(status, json!({"active":false,"retained_recordings":0}));
        platforms.cancel_sky_audio().unwrap();
    }
}

#[test]
fn sky_linux_audio_selects_one_configured_provider_and_returns_owned_wav() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("invocations");
    let pactl = executable(
        temp.path(),
        "owned-pactl",
        &format!(
            "import pathlib\np=pathlib.Path({:?});p.write_text(p.read_text()+'pactl\\n' if p.exists() else 'pactl\\n')\nprint('synthetic-sink')\n",
            log
        ),
    );
    let ffmpeg = executable(
        temp.path(),
        "owned-ffmpeg",
        "import sys,wave\nwith wave.open(sys.argv[-1],'wb') as f:\n f.setnchannels(2);f.setsampwidth(2);f.setframerate(24000);f.writeframes(bytes(96))\nprint('progress=continue',file=sys.stderr,flush=True)\nsys.stdin.readline()\n",
    );
    let mut platforms = Platforms::new();
    let params = json!({"max_duration_ms":100});
    assert!(
        platforms
            .sky_audio("start_audio_recording", &params)
            .unwrap_err()
            .message
            .contains("requires one")
    );
    assert!(!log.exists());
    for id in ["one", "two"] {
        platforms.configure(&json!({"id":id,"kind":"linux_audio","pactl":pactl,"ffmpeg":ffmpeg,"directory":temp.path().join(id)})).unwrap();
    }
    assert!(
        platforms
            .sky_audio("start_audio_recording", &params)
            .unwrap_err()
            .message
            .contains("ambiguous")
    );
    assert!(
        !log.exists(),
        "Ambiguous provider selection must not start a recorder"
    );
    platforms
        .execute("platform.unregister", &json!({"id":"two"}))
        .unwrap();
    platforms
        .sky_audio("start_audio_recording", &params)
        .unwrap();
    assert_eq!(
        platforms
            .sky_audio("start_audio_recording", &params)
            .unwrap_err()
            .message,
        "computer audio recording is already active"
    );
    let result = platforms
        .sky_audio("stop_audio_recording", &json!({}))
        .unwrap();
    let bytes = STANDARD.decode(result["data"].as_str().unwrap()).unwrap();
    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 24000);
    assert_eq!(u16::from_le_bytes(bytes[22..24].try_into().unwrap()), 2);
    assert_eq!(bytes.len(), 140);
    assert_eq!(result["mime_type"], "audio/wav");
    assert_eq!(
        fs::read(result["filepath"].as_str().unwrap()).unwrap(),
        bytes
    );
    platforms
        .sky_audio("start_audio_recording", &params)
        .unwrap();
    platforms.end_turn().unwrap();
    assert_eq!(
        platforms
            .sky_audio("stop_audio_recording", &json!({}))
            .unwrap_err()
            .message,
        "computer audio recording is not active"
    );
    assert_eq!(fs::read_to_string(log).unwrap().lines().count(), 2);
}
