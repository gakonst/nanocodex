//! Removed desktop extensions are not advertised and cannot reach the backend.
use serde_json::{Value, json};
use skyre::{
    Error, Result,
    ax::Node,
    engine::Engine,
    native::{Action, App, Desktop, Image},
    runtime::Host,
};
use std::{cell::RefCell, rc::Rc, time::Duration};

struct DesktopOnly {
    calls: Rc<RefCell<Vec<&'static str>>>,
    denied: bool,
}
impl Desktop for DesktopOnly {
    fn apps(&mut self) -> Result<Vec<App>> {
        panic!("desktop capture must not enumerate or bind apps")
    }
    fn snapshot(&mut self, _: &App) -> Result<Node> {
        panic!("desktop capture must not query accessibility")
    }
    fn action(&mut self, _: &App, _: Action) -> Result<()> {
        panic!("desktop capture must not change the UI")
    }
    fn desktop_screenshot(&mut self) -> Result<Image> {
        self.calls.borrow_mut().push("capture");
        if self.denied {
            return Err(Error::new(-32003, "Grant Screen Recording permission"));
        }
        Ok(Image {
            mime_type: "image/png".into(),
            data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=".into(),
        })
    }
    fn capabilities(&self) -> Vec<&'static str> {
        vec!["list_app_windows"]
    }
}
fn host(denied: bool) -> (Host, Rc<RefCell<Vec<&'static str>>>) {
    let calls = Rc::new(RefCell::new(vec![]));
    let engine = Rc::new(RefCell::new(Engine::new(Box::new(DesktopOnly {
        calls: calls.clone(),
        denied,
    }))));
    engine.borrow_mut().security.authorize_native_control();
    (Host::new(engine).unwrap(), calls)
}
fn eval(host: &mut Host, code: &str) -> Value {
    let result = host.evaluate(code, Duration::from_secs(3)).unwrap();
    assert!(result.get("error").is_none(), "{result}");
    result
}
#[test]
fn public_facade_does_not_expose_desktop_capture_or_window_extensions() {
    let (mut host, calls) = host(false);
    let result = eval(
        &mut host,
        "nodeRepl.write(JSON.stringify([typeof cua.getScreenshot,typeof cua.listWindows,typeof cua.computer.get_desktop_screenshot,typeof cua.computer.list_app_windows]));",
    );
    assert!(
        result["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["value"] == "[\"undefined\",\"undefined\",\"undefined\",\"undefined\"]"),
        "{result}"
    );
    assert!(calls.borrow().is_empty());
}
#[test]
fn removed_sky_capture_is_unavailable_without_calling_the_backend() {
    for denied in [false, true] {
        let calls = Rc::new(RefCell::new(vec![]));
        let mut engine = Engine::new(Box::new(DesktopOnly {
            calls: calls.clone(),
            denied,
        }));
        engine.security.authorize_native_control();
        let error = engine
            .execute(
                "sky.execute",
                &json!({"method":"get_desktop_screenshot","args":[]}),
            )
            .unwrap_err();
        assert_eq!(error.code, -32601);
        assert_eq!(
            error.message,
            "Sky method unavailable: get_desktop_screenshot"
        );
        assert!(calls.borrow().is_empty());
    }
}
#[test]
fn default_backend_explicitly_reports_unsupported_desktop_capture() {
    let mut engine = Engine::new(Box::new(skyre::fixture::Fixture::default()));
    engine.security.authorize_native_control();
    let error = engine
        .execute(
            "sky.execute",
            &json!({"method":"get_desktop_screenshot","args":[]}),
        )
        .unwrap_err();
    assert!(error.message.contains("unavailable"), "{error}");
}

#[test]
fn removed_capture_never_bypasses_restrictions_or_missing_host_grant() {
    use skyre::security::{Security, SecurityConfig};

    for (config, authorized, code) in [
        (
            SecurityConfig {
                allowed_apps: vec!["com.apple.finder".into()],
                ..Default::default()
            },
            true,
            -32601,
        ),
        (
            SecurityConfig {
                allowed_origins: vec!["https://allowed.example".into()],
                ..Default::default()
            },
            true,
            -32601,
        ),
        (
            SecurityConfig {
                denied_origins: vec!["https://denied.example".into()],
                ..Default::default()
            },
            true,
            -32601,
        ),
        (
            SecurityConfig {
                require_origin_approval: true,
                ..Default::default()
            },
            true,
            -32601,
        ),
        (SecurityConfig::default(), false, -32601),
        (
            SecurityConfig {
                preapproved_apps: vec!["com.apple.finder".into()],
                ..Default::default()
            },
            false,
            -32601,
        ),
    ] {
        let calls = Rc::new(RefCell::new(vec![]));
        let mut engine = Engine::new(Box::new(DesktopOnly {
            calls: calls.clone(),
            denied: false,
        }));
        engine.security = Security::new(config).unwrap();
        if authorized {
            engine.security.authorize_native_control();
        }
        let error = engine
            .execute(
                "sky.execute",
                &json!({
                    "method":"get_desktop_screenshot", "args":[]
                }),
            )
            .unwrap_err();
        assert_eq!(error.code, code);
        assert!(calls.borrow().is_empty());
    }
}
