#![cfg(unix)]
//! Executes only owned logging executables, never xdotool or a desktop.
use serde_json::{Value, json};
use skyre::{native::Desktop, platforms::linux::LinuxDesktop};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

fn fixture(path: &Path) -> std::path::PathBuf {
    let helper = path.join("owned_logger.py");
    fs::write(
        &helper,
        r#"#!/usr/bin/python3
import json,pathlib,sys
p=pathlib.Path(__file__).with_suffix('.log')
with p.open('a') as f:f.write(json.dumps(sys.argv[1:])+'\n')
if '999' in sys.argv:sys.exit(9)
"#,
    )
    .unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    helper
}
fn calls(path: &Path) -> Vec<Value> {
    fs::read_to_string(path.join("owned_logger.log"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
#[test]
fn linux_sky_full_desktop_paths_handles_and_failure_release_use_owned_processes() {
    let dir = tempfile::tempdir().unwrap();
    let helper = fixture(dir.path());
    let mut desktop = LinuxDesktop::new(helper.clone(), helper);
    assert_eq!(desktop.sky_target(), "linux");
    desktop
        .sky_execute(
            "drag",
            &json!({"path":[{"x":1,"y":2},{"x":3,"y":4},{"x":5,"y":6}],"key":"Shift"}),
        )
        .unwrap();
    assert_eq!(
        calls(dir.path()),
        vec![
            json!(["keydown", "Shift"]),
            json!(["mousemove", "--sync", "1", "2", "mousedown", "1"]),
            json!(["mousemove", "--sync", "3", "4"]),
            json!(["mousemove", "--sync", "5", "6"]),
            json!(["mouseup", "1"]),
            json!(["keyup", "Shift"])
        ]
    );
    assert!(
        desktop
            .sky_execute("drag_move", &json!({"handle_id":"a","point":{"x":1,"y":2}}))
            .is_err()
    );
    desktop
        .sky_execute(
            "drag_start",
            &json!({"handle_id":"a","point":{"x":1,"y":2}}),
        )
        .unwrap();
    assert!(
        desktop
            .sky_execute(
                "drag_start",
                &json!({"handle_id":"a","point":{"x":3,"y":4}})
            )
            .is_err()
    );
    assert!(
        desktop
            .sky_execute(
                "drag_start",
                &json!({"handle_id":"b","point":{"x":3,"y":4}})
            )
            .is_err()
    );
    desktop
        .sky_execute("drag_move", &json!({"handle_id":"a","point":{"x":5,"y":6}}))
        .unwrap();
    desktop.end_session("owner").unwrap();
    assert_eq!(calls(dir.path()).last().unwrap(), &json!(["mouseup", "1"]));
    assert!(
        desktop
            .sky_execute("drag_end", &json!({"handle_id":"a"}))
            .is_err()
    );
    assert!(
        desktop
            .sky_execute(
                "drag",
                &json!({"path":[{"x":1,"y":2},{"x":999,"y":4}],"key":"Control"})
            )
            .is_err()
    );
    let tail = calls(dir.path());
    assert_eq!(
        &tail[tail.len() - 2..],
        &[json!(["mouseup", "1"]), json!(["keyup", "Control"])]
    );
}
#[test]
fn linux_sky_scroll_optional_origin_and_modifier_cleanup_are_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let helper = fixture(dir.path());
    let mut desktop = LinuxDesktop::new(helper.clone(), helper);
    desktop
        .sky_execute(
            "scroll",
            &json!({"direction":" D ","pixels":200,"key":"Alt"}),
        )
        .unwrap();
    assert_eq!(
        calls(dir.path()),
        vec![
            json!(["keydown", "Alt"]),
            json!(["click", "--repeat", "2", "--delay", "10", "5"]),
            json!(["keyup", "Alt"])
        ]
    );
    let before = calls(dir.path()).len();
    assert!(
        desktop
            .sky_execute("drag", &json!({"path":[{"x":1,"y":2},{"x":3}],"key":"Alt"}))
            .is_err()
    );
    assert_eq!(calls(dir.path()).len(), before);
}
#[test]
fn linux_installed_screenshot_wire_is_jpeg_from_owned_synthetic_pixels() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let dir = tempfile::tempdir().unwrap();
    let tool = fixture(dir.path());
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbImage::from_pixel(3, 2, image::Rgb([20, 100, 180]))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    fs::write(dir.path().join("pixels.png"), png.into_inner()).unwrap();
    let capture = dir.path().join("owned_pixels.py");
    fs::write(&capture,"#!/usr/bin/python3\nimport pathlib,sys\nsys.stdout.buffer.write(pathlib.Path(__file__).with_name('pixels.png').read_bytes())\n").unwrap();
    fs::set_permissions(&capture, fs::Permissions::from_mode(0o700)).unwrap();
    let mut desktop = LinuxDesktop::new(tool, capture);
    let result = desktop.sky_execute("get_screenshot", &json!({})).unwrap();
    assert_eq!(result[0]["mime_type"], "image/jpeg");
    let encoded = STANDARD
        .decode(result[0]["data"].as_str().unwrap())
        .unwrap();
    assert_eq!(&encoded[..3], &[255, 216, 255]);
    let decoded = image::load_from_memory(&encoded).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (3, 2));
}

#[test]
fn linux_app_modified_drags_refuse_before_any_helper_or_focus_change() {
    use skyre::native::{Action, App};
    let dir = tempfile::tempdir().unwrap();
    let helper = fixture(dir.path());
    let mut desktop = LinuxDesktop::new(helper.clone(), helper);
    let app = App {
        window_id: None,
        id: "x11:123".into(),
        name: "owned fixture".into(),
        path: "x11:123".into(),
        pid: 123,
    };
    for (button, modifiers) in [(2, vec![]), (0, vec!["shift".into()])] {
        let error = desktop
            .action(
                &app,
                Action::Drag {
                    from: [1., 2.],
                    to: [3., 4.],
                    button,
                    modifiers,
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("does not support modified"));
        assert!(!dir.path().join("owned_logger.log").exists());
    }
}
