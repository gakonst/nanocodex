//! Generator DTO integration through the real Engine with an inert Desktop.
//! macOS-only cases read disposable owned Info.plist data without loading code.
use serde_json::json;
use skyre::{
    Error, Result,
    ax::Node,
    engine::Engine,
    native::{Action, App, Desktop},
};
use std::{cell::RefCell, rc::Rc};

#[derive(Default)]
struct State {
    guidance: Option<String>,
    reads: usize,
    snapshots: usize,
    fail_snapshot: bool,
}
struct Provider(Rc<RefCell<State>>);
fn app() -> App {
    App {
        id: "owned.instructions".into(),
        name: "Owned instructions".into(),
        path: "fixture://instructions".into(),
        pid: 41,
    }
}
impl Desktop for Provider {
    fn synthetic(&self) -> bool {
        true
    }
    fn apps(&mut self) -> Result<Vec<App>> {
        Ok(vec![app()])
    }
    fn snapshot(&mut self, _: &App) -> Result<Node> {
        let mut state = self.0.borrow_mut();
        state.snapshots += 1;
        if state.fail_snapshot {
            return Err(Error::action("owned snapshot failure"));
        }
        Ok(Node {
            identity: "owned-window".into(),
            role: "AXWindow".into(),
            title: Some("Owned window".into()),
            ..Default::default()
        })
    }
    fn app_specific_instructions(&mut self, observed: &App) -> Option<String> {
        assert_eq!(observed.id, app().id);
        assert_eq!(observed.path, app().path);
        let mut state = self.0.borrow_mut();
        state.reads += 1;
        state.guidance.clone()
    }
    fn action(&mut self, _: &App, _: Action) -> Result<()> {
        Err(Error::action("fixture actions are not permitted"))
    }
    fn capabilities(&self) -> Vec<&'static str> {
        vec!["get_app_state"]
    }
}

#[test]
fn engine_returns_native_guidance_only_after_capture_and_preserves_absence() {
    let state = Rc::new(RefCell::new(State {
        fail_snapshot: true,
        guidance: Some(" body μ\n".into()),
        ..Default::default()
    }));
    let mut engine = Engine::new(Box::new(Provider(state.clone())));
    let request = json!({"app":app().path});
    let error = engine.execute("get_app_state", &request).unwrap_err();
    assert!(error.message.contains("owned snapshot failure"));
    assert_eq!(state.borrow().reads, 0);
    state.borrow_mut().fail_snapshot = false;
    let value = engine.execute("get_app_state", &request).unwrap();
    assert_eq!(value["appSpecificInstructions"], " body μ\n");
    assert_eq!(value["bundleIdentifier"], app().id);
    assert_eq!(state.borrow().reads, 1);
    state.borrow_mut().guidance = None;
    let value = engine.execute("get_app_state", &request).unwrap();
    assert!(value.get("appSpecificInstructions").is_none());
    assert_eq!(state.borrow().reads, 2);
}

#[test]
fn approved_sky_dto_preserves_guidance_without_a_generator_cache() {
    let state = Rc::new(RefCell::new(State {
        guidance: Some("owned first\n".into()),
        ..Default::default()
    }));
    let mut engine = Engine::new(Box::new(Provider(state.clone())));
    let request = json!({"method":"get_app_state","args":[{"app":app().path}]});
    assert!(engine.execute("sky.execute", &request).is_err());
    assert_eq!(state.borrow().snapshots, 0);
    assert_eq!(state.borrow().reads, 0);
    engine
        .execute("sky.app_policy", &json!({"app":app().path}))
        .unwrap();
    let approval = json!({"meta":{"connector_id":"computer-use","tool_name":"get_app_state","tool_params":{"app":app().id}}});
    assert_eq!(
        engine.execute("host.elicitation", &approval).unwrap()["action"],
        "accept"
    );
    let value = engine.execute("sky.execute", &request).unwrap();
    assert_eq!(value["appSpecificInstructions"], "owned first\n");
    assert_eq!(value["app"]["bundleIdentifier"], app().id);
    assert!(value["skyshot"]["screenshot"].is_null());
    state.borrow_mut().guidance = Some("owned second".into());
    let next = engine.execute("sky.execute", &request).unwrap();
    assert_eq!(next["appSpecificInstructions"], "owned second");
    assert_eq!(state.borrow().reads, 2);
    state.borrow_mut().guidance = None;
    let absent = engine.execute("sky.execute", &request).unwrap();
    assert!(absent.get("appSpecificInstructions").is_none());
    assert_eq!(state.borrow().reads, 3);
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use skyre::native::macos::MacDesktop;

    fn bundle(name: &str, url_types: &str) -> (tempfile::TempDir, App) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Owned.app");
        std::fs::create_dir_all(path.join("Contents")).unwrap();
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>CFBundleIdentifier</key><string>metadata.identifier</string>
<key>CFBundlePackageType</key><string>APPL</string><key>CFBundleDisplayName</key><string>Slack</string>
<key>CFBundleName</key>{name}<key>CFBundleURLTypes</key>{url_types}</dict></plist>"#
        );
        std::fs::write(path.join("Contents/Info.plist"), plist).unwrap();
        assert!(!path.join("Contents/MacOS").exists());
        (
            directory,
            App {
                id: "owned.controller".into(),
                name: "Slack".into(),
                path: path.to_str().unwrap().into(),
                pid: 0,
            },
        )
    }

    #[test]
    fn owned_bundle_name_and_controller_identifier_are_distinct_candidates() {
        let mut desktop = MacDesktop::new();
        let (_directory, mut app) = bundle("<string>Spotify</string>", "<array/>");
        assert_eq!(
            desktop.app_specific_instructions(&app).unwrap().as_bytes(),
            include_bytes!("../src/native/app_instructions/Spotify.md")
        );
        app.id = "com.apple.Music".into();
        assert_eq!(
            desktop.app_specific_instructions(&app).unwrap().as_bytes(),
            include_bytes!("../src/native/app_instructions/AppleMusic.md")
        );
        let (_directory, mut app) = bundle("<integer>1</integer>", "<array/>");
        assert!(
            desktop.app_specific_instructions(&app).is_none(),
            "display name is not a candidate"
        );
        app.id = "Slack".into();
        assert_eq!(
            desktop.app_specific_instructions(&app).unwrap().as_bytes(),
            include_bytes!("../src/native/app_instructions/Slack.md")
        );
        app.path = _directory
            .path()
            .join("Missing.app")
            .to_str()
            .unwrap()
            .into();
        app.id = "com.apple.Music".into();
        assert_eq!(
            desktop.app_specific_instructions(&app).unwrap().as_bytes(),
            include_bytes!("../src/native/app_instructions/AppleMusic.md")
        );
    }

    #[test]
    fn owned_bundle_http_declarations_require_exact_string_and_container_shapes() {
        let mut desktop = MacDesktop::new();
        let browser = include_str!("../src/native/app_instructions/Browser.md");
        for (types, expected) in [
            (
                "<array><dict><key>CFBundleURLSchemes</key><array><string>http</string></array></dict></array>",
                true,
            ),
            (
                "<array><dict><key>CFBundleURLSchemes</key><array><string>https</string><string>HTTP</string></array></dict></array>",
                false,
            ),
            (
                "<array><dict><key>CFBundleURLSchemes</key><array><string>http</string><integer>1</integer></array></dict></array>",
                false,
            ),
            (
                "<array><dict><key>CFBundleURLSchemes</key><array><string>http</string></array></dict><integer>1</integer></array>",
                false,
            ),
            (
                "<array><dict><key>CFBundleURLSchemes</key><integer>1</integer></dict><dict><key>CFBundleURLSchemes</key><array><string>http</string></array></dict></array>",
                true,
            ),
            ("<string>http</string>", false),
        ] {
            let (_directory, app) = bundle("<string>Unmapped</string>", types);
            assert_eq!(
                desktop.app_specific_instructions(&app).as_deref(),
                expected.then_some(browser),
                "{types}"
            );
        }
        let (_directory, app) = bundle(
            "<string>Spotify</string>",
            "<array><dict><key>CFBundleURLSchemes</key><array><string>http</string></array></dict></array>",
        );
        assert_eq!(
            desktop.app_specific_instructions(&app).unwrap(),
            format!(
                "{browser}\n\n{}",
                include_str!("../src/native/app_instructions/Spotify.md")
            )
        );
    }
}
