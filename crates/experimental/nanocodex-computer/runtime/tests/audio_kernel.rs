use serde_json::{Value, json};
use skyre::{
    Error, Result,
    ax::Node,
    engine::Engine,
    host_turns::Route,
    native::{Action, App, Desktop},
};
use std::{cell::RefCell, rc::Rc};

#[derive(Default)]
struct State {
    active: bool,
    fail_cancel: bool,
    fail_stop: bool,
    cancels: usize,
    sessions: usize,
}
struct Recorder(Rc<RefCell<State>>);
impl Desktop for Recorder {
    fn synthetic(&self) -> bool {
        true
    }
    fn apps(&mut self) -> Result<Vec<App>> {
        Ok(vec![])
    }
    fn snapshot(&mut self, _: &App) -> Result<Node> {
        Err(Error::unsupported("unused"))
    }
    fn action(&mut self, _: &App, _: Action) -> Result<()> {
        Err(Error::unsupported("unused"))
    }
    fn capabilities(&self) -> Vec<&'static str> {
        vec![]
    }
    fn audio(&mut self, method: &str, _: &str, _: &Value) -> Result<Value> {
        let mut state = self.0.borrow_mut();
        match method {
            "start" => {
                if state.active {
                    return Err(Error::action("already active"));
                }
                state.active = true;
                Ok(Value::Null)
            }
            "stop" => {
                if state.fail_stop {
                    return Err(Error::action("owned provider stop failed"));
                }
                if !state.active {
                    return Err(Error::action("inactive"));
                }
                state.active = false;
                Ok(json!({"data":"","mime_type":"audio/wav"}))
            }
            "status" => Ok(json!({"active":state.active})),
            _ => Err(Error::unsupported("unused")),
        }
    }
    fn cancel_audio(&mut self, _: &str) -> Result<()> {
        let mut state = self.0.borrow_mut();
        state.cancels += 1;
        if state.fail_cancel {
            return Err(Error::action("owned provider cancel failed"));
        }
        state.active = false;
        Ok(())
    }
    fn end_session(&mut self, _: &str) -> Result<()> {
        let mut state = self.0.borrow_mut();
        state.active = false;
        state.sessions += 1;
        Ok(())
    }
}
fn route(id: &str) -> Route {
    Route {
        conversation_id: id.into(),
        thread_id: None,
    }
}
fn setup() -> (Engine, Rc<RefCell<State>>) {
    let state = Rc::new(RefCell::new(State::default()));
    (Engine::new(Box::new(Recorder(state.clone()))), state)
}
#[test]
fn recording_is_owned_by_kernel_and_reset_does_not_end_external_session() {
    let (mut engine, state) = setup();
    engine.select_kernel_scope(&route("a"));
    engine
        .execute("audio.start", &json!({"scope":"system"}))
        .unwrap();
    engine.select_kernel_scope(&route("b"));
    for method in ["audio.start", "audio.stop", "audio.status"] {
        assert_eq!(
            engine
                .execute(method, &json!({"scope":"system"}))
                .unwrap_err()
                .code,
            -32001
        );
    }
    engine.reset_kernel_resources().unwrap();
    assert!(state.borrow().active);
    assert_eq!(state.borrow().cancels, 0);
    engine.select_kernel_scope(&route("a"));
    engine.reset_kernel_resources().unwrap();
    assert!(!state.borrow().active);
    assert_eq!(state.borrow().cancels, 1);
    assert_eq!(state.borrow().sessions, 0);
    engine.reset_kernel_resources().unwrap();
    assert_eq!(state.borrow().cancels, 1);
    engine
        .execute("audio.start_audio_recording", &json!({"scope":"system"}))
        .unwrap();
    engine
        .execute("audio.stop_audio_recording", &json!({}))
        .unwrap();
    engine.reset_kernel_resources().unwrap();
    assert_eq!(state.borrow().cancels, 1);
}
#[test]
fn failed_stop_or_cancel_retains_owner_until_cleanup_succeeds() {
    let (mut engine, state) = setup();
    engine.select_kernel_scope(&route("a"));
    engine
        .execute("audio.start", &json!({"scope":"system"}))
        .unwrap();
    state.borrow_mut().fail_stop = true;
    assert!(engine.execute("audio.stop", &json!({})).is_err());
    state.borrow_mut().fail_cancel = true;
    assert!(engine.reset_kernel_resources().is_err());
    engine.select_kernel_scope(&route("b"));
    assert_eq!(
        engine.execute("audio.stop", &json!({})).unwrap_err().code,
        -32001
    );
    engine.select_kernel_scope(&route("a"));
    state.borrow_mut().fail_cancel = false;
    engine.reset_kernel_resources().unwrap();
    assert!(!state.borrow().active);
    engine.select_kernel_scope(&route("b"));
    engine
        .execute("audio.start", &json!({"scope":"system"}))
        .unwrap();
    engine.end_session();
    assert!(!state.borrow().active);
    assert_eq!(state.borrow().sessions, 1);
}

#[test]
fn control_revocation_discards_capture_and_does_not_leave_a_false_active_owner() {
    let (mut engine, state) = setup();
    engine.guardian.set_host_state(false, 1).unwrap();
    let lease = engine
        .execute("guardian.acquire", &json!({"ttl_ms":1000}))
        .unwrap();
    engine
        .execute("audio.start", &json!({"scope":"system"}))
        .unwrap();
    engine
        .execute("guardian.release", &json!({"lease":lease["lease"]}))
        .unwrap();
    assert!(!state.borrow().active);
    engine
        .execute("audio.start", &json!({"scope":"system"}))
        .unwrap();
    engine.reset_kernel_resources().unwrap();
    assert!(!state.borrow().active);
}
