//! Inert provider verification of observation geometry publication and rollback.
use serde_json::json;
use skyre::{
    Error, Result,
    ax::Node,
    engine::Engine,
    native::{Action, App, Desktop, Image},
};
use std::{cell::RefCell, rc::Rc};

#[derive(Default)]
struct State {
    geometry: bool,
    fail_snapshot: bool,
    fail_capture: bool,
    bad_media: bool,
    observation_captures: usize,
    ordinary_captures: usize,
}
struct Provider(Rc<RefCell<State>>);
fn app() -> App {
    App {
        id: "owned.capture".into(),
        name: "Owned capture".into(),
        path: "fixture://capture".into(),
        pid: 42,
    }
}
fn image(state: &State) -> Image {
    Image {
        mime_type: "image/png".into(),
        data: if state.bad_media {
            "invalid-base64!"
        } else {
            "AA=="
        }
        .into(),
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
        if self.0.borrow().fail_snapshot {
            return Err(Error::action("owned snapshot failure"));
        }
        Ok(Node {
            identity: "window".into(),
            role: "AXWindow".into(),
            title: Some("Owned window".into()),
            ..Default::default()
        })
    }
    fn action(&mut self, _: &App, _: Action) -> Result<()> {
        Ok(())
    }
    fn screenshot(&mut self, _: &App) -> Result<Image> {
        let mut state = self.0.borrow_mut();
        state.ordinary_captures += 1;
        Ok(image(&state))
    }
    fn screenshot_for_observation(&mut self, _: &App) -> Result<Image> {
        let mut state = self.0.borrow_mut();
        state.observation_captures += 1;
        // Deliberately stage before an optional failure. Engine must invalidate it.
        state.geometry = true;
        if state.fail_capture {
            return Err(Error::action("owned capture failure"));
        }
        Ok(image(&state))
    }
    fn invalidate_screenshot(&mut self, _: &App) {
        self.0.borrow_mut().geometry = false;
    }
    fn capabilities(&self) -> Vec<&'static str> {
        vec!["get_screenshot"]
    }
}
fn fixture() -> (Engine, Rc<RefCell<State>>) {
    let state = Rc::new(RefCell::new(State::default()));
    (Engine::new(Box::new(Provider(state.clone()))), state)
}
fn observe(engine: &mut Engine) -> Result<serde_json::Value> {
    engine.execute(
        "get_app_state",
        &json!({"app":app().path,"screenshot":true}),
    )
}

#[test]
fn observation_capture_is_separate_from_preview_and_failed_capture_revokes_geometry() {
    let (mut engine, state) = fixture();
    observe(&mut engine).unwrap();
    assert!(state.borrow().geometry);
    engine
        .execute("get_screenshot", &json!({"app":app().path}))
        .unwrap();
    assert!(state.borrow().geometry);
    assert_eq!(state.borrow().observation_captures, 1);
    assert_eq!(state.borrow().ordinary_captures, 1);
    state.borrow_mut().fail_snapshot = true;
    assert!(observe(&mut engine).is_err());
    assert!(!state.borrow().geometry);
    engine
        .execute("get_screenshot", &json!({"app":app().path}))
        .unwrap();
    assert!(
        !state.borrow().geometry,
        "ordinary capture cannot establish observation geometry"
    );
    state.borrow_mut().fail_snapshot = false;
    state.borrow_mut().fail_capture = true;
    assert!(observe(&mut engine).is_err());
    assert!(!state.borrow().geometry);
    state.borrow_mut().fail_capture = false;
    observe(&mut engine).unwrap();
    assert!(state.borrow().geometry);
    engine
        .execute("get_app_state", &json!({"app":app().path}))
        .unwrap();
    assert!(
        !state.borrow().geometry,
        "AX-only observation cannot retain previous screenshot geometry"
    );
}

#[test]
fn failed_revision_and_media_publication_cannot_leave_coordinate_authority() {
    let (mut engine, state) = fixture();
    observe(&mut engine).unwrap();
    engine
        .sessions
        .revisions
        .get_mut(&app().path)
        .unwrap()
        .generation = u64::MAX;
    let error = observe(&mut engine).unwrap_err();
    assert!(error.message.contains("generation overflow"), "{error}");
    assert!(!state.borrow().geometry);
    engine.sessions.revisions.clear();
    engine
        .execute("sky.app_policy", &json!({"app":app().path}))
        .unwrap();
    let (_,approval)=engine.prepare_elicitation(&json!({"meta":{"connector_id":"computer-use","tool_name":"get_app_state","tool_params":{"app":app().id}}})).unwrap();
    assert_eq!(approval.unwrap()["action"], "accept");
    state.borrow_mut().bad_media = true;
    let error = engine
        .execute(
            "sky.execute",
            &json!({"method":"get_app_state","args":[{"app":app().path}]}),
        )
        .unwrap_err();
    assert!(error.message.contains("media encoding"), "{error}");
    assert!(!state.borrow().geometry);
}

#[test]
fn optional_capture_preserves_ax_state_and_original_error_without_geometry() {
    let (engine, state) = fixture();
    state.borrow_mut().fail_capture = true;
    let mut host = skyre::runtime::Host::new(Rc::new(RefCell::new(engine))).unwrap();
    let result = host.evaluate(
        r#"
        var app = await cua.getApp('fixture://capture');
        var ax = await app.getAXState({emit:false});
        var combined = await app.getAXStateAndScreenshot({emit:false});
        var captureError;
        try { await app.getScreenshot({emit:false}); }
        catch (e) { captureError = {message:e.message,code:e.code}; }
        nodeRepl.write(JSON.stringify({hasAX:ax.includes('Owned window'),hasCombined:combined.state.includes('Owned window'),hasScreenshot:'screenshot' in combined,captureError}));
        "#,
        std::time::Duration::from_secs(3),
    ).unwrap();
    assert!(result.get("error").is_none(), "{result}");
    let expected = json!({
        "hasAX":true,"hasCombined":true,"hasScreenshot":false,
        "captureError":{"message":"owned capture failure","code":Error::action("unused").code}
    });
    assert!(
        result["outputs"].as_array().unwrap().iter().any(|o| {
            o["value"]
                .as_str()
                .and_then(|v| serde_json::from_str::<serde_json::Value>(v).ok())
                == Some(expected.clone())
        }),
        "{result}"
    );
    assert!(
        !state.borrow().geometry,
        "failed optional capture grants no coordinate authority"
    );
    assert_eq!(state.borrow().observation_captures, 4);
    assert_eq!(state.borrow().ordinary_captures, 0);
}

#[test]
fn sky_optional_capture_keeps_error_and_ax_revision_but_explicit_capture_is_strict() {
    let (mut engine, state) = fixture();
    state.borrow_mut().fail_capture = true;
    let result = engine
        .execute(
            "get_app_state",
            &json!({
                "app":app().path,"screenshot":true,"screenshotOptional":true
            }),
        )
        .unwrap();
    assert!(result["state"].as_str().unwrap().contains("Owned window"));
    assert!(result["screenshot"].is_null());
    assert_eq!(
        result["screenshotError"],
        json!({"message":"owned capture failure","code":Error::action("unused").code})
    );
    assert!(!state.borrow().geometry);
    let error = observe(&mut engine).unwrap_err();
    assert_eq!(error.message, "owned capture failure");
    assert!(!state.borrow().geometry);
}
