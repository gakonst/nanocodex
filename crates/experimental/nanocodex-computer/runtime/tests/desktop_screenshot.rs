//! Display capture must work with no apps or AX windows, and never bind action geometry.
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
        vec![]
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
fn desktop_screenshot_is_read_only_and_emits_once_by_default() {
    let (mut host, calls) = host(false);
    let result = eval(
        &mut host,
        "var shot = await cua.getScreenshot(); nodeRepl.write([shot instanceof Uint8Array, shot[0], shot[1]]);",
    );
    let outputs = result["outputs"].as_array().unwrap();
    assert_eq!(
        outputs.iter().filter(|o| o["channel"] == "image").count(),
        1
    );
    assert!(outputs.iter().any(|o| o["value"] == "[ true, 137, 80 ]"));
    assert_eq!(&*calls.borrow(), &["capture"]);
    let result = eval(&mut host, "await cua.getScreenshot({emit:false});");
    assert!(
        !result["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["channel"] == "image")
    );
    assert_eq!(&*calls.borrow(), &["capture", "capture"]);
}
#[test]
fn desktop_screenshot_keeps_permission_errors_actionable() {
    let (mut host, calls) = host(true);
    let result = eval(
        &mut host,
        "try { await cua.getScreenshot(); } catch (e) { nodeRepl.write(JSON.stringify({message:e.message,code:e.code})); }",
    );
    assert!(
        result["outputs"].as_array().unwrap().iter().any(|o| {
            o["value"]
                .as_str()
                .and_then(|v| serde_json::from_str::<Value>(v).ok())
                == Some(json!({"message":"Grant Screen Recording permission","code":-32003}))
        }),
        "{result}"
    );
    assert_eq!(&*calls.borrow(), &["capture"]);
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
fn restrictions_and_missing_host_grant_block_direct_and_facade_display_capture() {
    use skyre::security::{Security, SecurityConfig};

    for (config, authorized, code) in [
        (
            SecurityConfig {
                allowed_apps: vec!["com.apple.finder".into()],
                ..Default::default()
            },
            true,
            -32010,
        ),
        (
            SecurityConfig {
                allowed_origins: vec!["https://allowed.example".into()],
                ..Default::default()
            },
            true,
            -32010,
        ),
        (
            SecurityConfig {
                denied_origins: vec!["https://denied.example".into()],
                ..Default::default()
            },
            true,
            -32010,
        ),
        (
            SecurityConfig {
                require_origin_approval: true,
                ..Default::default()
            },
            true,
            -32010,
        ),
        (SecurityConfig::default(), false, -32003),
        (
            SecurityConfig {
                preapproved_apps: vec!["com.apple.finder".into()],
                ..Default::default()
            },
            false,
            -32003,
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

        let mut host = Host::new(Rc::new(RefCell::new(engine))).unwrap();
        let result = eval(
            &mut host,
            r#"
        try { await cua.getScreenshot(); }
        catch (error) { nodeRepl.write(JSON.stringify({message:error.message,code:error.code})); }
    "#,
        );
        let outputs = result["outputs"].as_array().unwrap();
        assert!(!outputs.iter().any(|o| o["channel"] == "image"));
        assert!(
            outputs.iter().any(|o| {
                o["value"]
                    .as_str()
                    .and_then(|v| serde_json::from_str::<Value>(v).ok())
                    == Some(json!({"message":error.message,"code":error.code}))
            }),
            "{result}"
        );
        assert!(calls.borrow().is_empty());
    }
}
