//! Synthetic transport, actual Rust admission/ownership state machine.
use serde_json::{Value, json};
use skyre::{
    browser::Browsers,
    security::{Security, SecurityConfig},
};
use std::{
    net::TcpListener,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};
use tungstenite::Message;
#[derive(Default)]
struct State {
    requests: Vec<Value>,
    status: String,
    url: Option<String>,
    drop_continue: bool,
    source_changed: bool,
    source_reads: usize,
    filename: Option<String>,
    frame: Option<String>,
    outer_error: Option<Value>,
    metadata_delay_ms: u64,
    fail_disable: bool,
    navigation_error: Option<String>,
    paused_during_disable: bool,
    inactive_continue: bool,
    html_response: bool,
    html_close: Option<HtmlClose>,
}
struct Provider {
    endpoint: String,
    state: Arc<Mutex<State>>,
    join: Option<thread::JoinHandle<()>>,
}
impl Provider {
    fn new() -> Self {
        let socket = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("ws://{}", socket.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State {
            status: "completed".into(),
            ..Default::default()
        }));
        let shared = state.clone();
        let join = thread::spawn(move || {
            let (stream, _) = socket.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            let mut deferred = None;
            let mut guid = 0;
            while let Ok(message) = ws.read() {
                let Ok(text) = message.to_text() else { break };
                let request: Value = serde_json::from_str(text).unwrap();
                let mut state = shared.lock().unwrap();
                state.requests.push(request.clone());
                let method = request["method"].as_str().unwrap();
                if method == "Target.closeTarget"
                    && matches!(state.html_close, Some(HtmlClose::Armed))
                {
                    state.html_close = Some(HtmlClose::AwaitingContinuation);
                    ws.send(Message::text(json!({"method":"Fetch.requestPaused","sessionId":"root","params":{"requestId":"late-close-document","request":{"url":"http://owned.test/"},"frameId":"frame","resourceType":"Document","responseStatusCode":200,"responseHeaders":[{"name":"Content-Type","value":"text/html"}]}}).to_string())).unwrap();
                    ws.send(Message::text(json!({"id":request["id"],"result":{"success":true,"owned":"actual-public-close-ack"}}).to_string())).unwrap();
                    continue;
                }
                if method == "Fetch.continueResponse"
                    && matches!(state.html_close, Some(HtmlClose::AwaitingContinuation))
                {
                    state.html_close = Some(HtmlClose::Closed);
                    ws.send(Message::text(json!({"method":"Target.detachedFromTarget","params":{"sessionId":"root","targetId":"t"}}).to_string())).unwrap();
                    ws.send(Message::text(json!({"id":request["id"],"error":{"code":-32001,"message":"Session with given id not found."}}).to_string())).unwrap();
                    continue;
                }
                if method == "Target.getTargets"
                    && matches!(state.html_close, Some(HtmlClose::Closed))
                {
                    drop(state);
                    ws.send(Message::text(
                        json!({"id":request["id"],"result":{"targetInfos":[]}}).to_string(),
                    ))
                    .unwrap();
                    continue;
                }
                if method == "Fetch.disable" && state.paused_during_disable {
                    state.paused_during_disable = false;
                    state.inactive_continue = true;
                    ws.send(Message::text(json!({"method":"Fetch.requestPaused","sessionId":"root","params":{"requestId":"late-document","request":{"url":"http://owned.test/"},"frameId":"frame","resourceType":"Document","responseStatusCode":200,"responseHeaders":[{"name":"Content-Type","value":"text/html"}]}}).to_string())).unwrap();
                    let response = if state.fail_disable {
                        state.fail_disable = false;
                        json!({"id":request["id"],"error":{"code":-32000,"message":"Owned disable refusal"}})
                    } else {
                        json!({"id":request["id"],"result":{}})
                    };
                    ws.send(Message::text(response.to_string())).unwrap();
                    continue;
                }
                if method == "Fetch.continueResponse" && state.inactive_continue {
                    state.inactive_continue = false;
                    ws.send(Message::text(json!({"id":request["id"],"error":{"code":-32000,"message":"Fetch domain is not enabled"}}).to_string())).unwrap();
                    continue;
                }
                if method == "Fetch.disable" && state.fail_disable {
                    state.fail_disable = false;
                    ws.send(Message::text(json!({"id":request["id"],"error":{"code":-32000,"message":"Owned disable refusal"}}).to_string())).unwrap();
                    continue;
                }
                if method == "Owned.fire" || method == "Owned.fire_after_ack" {
                    guid += 1;
                    deferred = (method == "Owned.fire").then(|| request["id"].clone());
                    let url = state.url.clone().unwrap_or("http://owned.test/file".into());
                    let event = json!({"method":"Fetch.requestPaused","sessionId":"root","params":{"requestId":format!("request-{guid}"),"request":{"url":url},"frameId":state.frame.as_deref().unwrap_or("frame"),"resourceType":"Document","responseStatusCode":200,"responseHeaders":[{"name":"Content-Type","value":if state.html_response {"text/html"} else {"application/octet-stream"}}]}});
                    drop(state);
                    if method == "Owned.fire_after_ack" {
                        ws.send(Message::text(
                            json!({"id":request["id"],"result":{}}).to_string(),
                        ))
                        .unwrap();
                    }
                    ws.send(Message::text(event.to_string())).unwrap();
                    continue;
                }
                if method == "Fetch.continueResponse" {
                    if state.drop_continue {
                        break;
                    }
                    let url = state.url.clone().unwrap_or("http://owned.test/file".into());
                    ws.send(Message::text(json!({"method":"Browser.downloadWillBegin","params":{"guid":format!("guid-{guid}"),"frameId":state.frame.as_deref().unwrap_or("frame"),"url":url,"suggestedFilename":"owned.bin"}}).to_string())).unwrap();
                    if !state.status.is_empty() {
                        ws.send(Message::text(json!({"method":"Browser.downloadProgress","params":{"guid":format!("guid-{guid}"),"state":state.status,"filePath":state.filename}}).to_string())).unwrap();
                    }
                }
                if matches!(method, "Fetch.continueResponse" | "Fetch.failRequest")
                    && let Some(id) = deferred.take()
                {
                    let response = if let Some(error) = &state.outer_error {
                        json!({"id":id,"error":error})
                    } else {
                        json!({"id":id,"result":{"outer":"acknowledged"}})
                    };
                    ws.send(Message::text(response.to_string())).unwrap();
                }
                let result = match method {
                    "Target.createTarget" => json!({"targetId":"t"}),
                    "Target.closeTarget" => json!({"success":true}),
                    "Page.navigate" => json!({"errorText":state.navigation_error}),
                    "Target.attachToTarget" => json!({"sessionId":"root"}),
                    "Page.getFrameTree" => {
                        json!({"frameTree":{"frame":{"id":"frame","url":"http://owned.test/","loaderId":"loader"}}})
                    }
                    "Target.getTargetInfo" => {
                        thread::sleep(Duration::from_millis(state.metadata_delay_ms));
                        state.source_reads += 1;
                        json!({"targetInfo":{"targetId":"t","url":if state.source_changed&&state.source_reads>=2{"http://changed.test/"}else{"http://owned.test/"}}})
                    }
                    _ => json!({}),
                };
                drop(state);
                if ws
                    .send(Message::text(
                        json!({"id":request["id"],"result":result}).to_string(),
                    ))
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            endpoint,
            state,
            join: Some(join),
        }
    }
    fn browser(&self) -> Browsers {
        let mut b = Browsers::default();
        b.set_download_security(
            Security::new(SecurityConfig {
                preapproved_download_origins: vec!["http://owned.test".into()],
                ..Default::default()
            })
            .unwrap(),
        );
        b.register("owned", &self.endpoint).unwrap();
        b
    }
    fn calls(&self, method: &str) -> Vec<Value> {
        self.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r["method"] == method)
            .cloned()
            .collect()
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
    }
}
fn arm(b: &mut Browsers, ms: Value) -> Value {
    b.execute("download_arm", &json!({"tab":"t","timeoutMs":ms}))
        .unwrap()
}
fn fire(b: &mut Browsers) -> skyre::Result<Value> {
    b.execute(
        "cdp_call",
        &json!({"tab":"t","method":"Owned.fire","params":{}}),
    )
}
fn poll(b: &mut Browsers, w: &Value) -> skyre::Result<Value> {
    b.execute("download_poll", &json!({"tab":"t","watchId":w["watchId"]}))
}
#[test]
fn download_paused_navigation_preserves_outer_ack_and_causal_completion() {
    let p = Provider::new();
    p.state.lock().unwrap().filename = Some("/owned/provider-reported.bin".into());
    let mut b = p.browser();
    let w = arm(&mut b, json!(1000));
    assert_eq!(fire(&mut b).unwrap()["outer"], "acknowledged");
    assert_eq!(poll(&mut b, &w).unwrap()["value"]["guid"], "guid-1");
    assert_eq!(
        b.execute("download_path", &json!({"tab":"t","downloadId":"guid-1"}))
            .unwrap()["path"],
        "/owned/provider-reported.bin"
    );
    assert!(p.calls("Fetch.disable").is_empty());
    assert_eq!(
        p.calls("Browser.setDownloadBehavior").last().unwrap()["params"]["eventsEnabled"],
        false
    );
    drop(b);
}
#[test]
fn download_path_missing_provider_filename_is_null_and_old_records_do_not_satisfy_new_wait() {
    let p = Provider::new();
    let mut b = p.browser();
    let w = arm(&mut b, json!(1000));
    fire(&mut b).unwrap();
    poll(&mut b, &w).unwrap();
    assert_eq!(
        b.execute("download_path", &json!({"tab":"t","downloadId":"guid-1"}))
            .unwrap(),
        json!({"path":null})
    );
    let next = arm(&mut b, json!(0));
    assert_eq!(
        poll(&mut b, &next).unwrap_err().message,
        "Timed out after 0ms waiting for download."
    );
    drop(b);
}
#[test]
fn download_cancellation_is_consumed_and_does_not_poison_next_wait() {
    let p = Provider::new();
    p.state.lock().unwrap().status = "canceled".into();
    let mut b = p.browser();
    let w = arm(&mut b, json!(1000));
    fire(&mut b).unwrap();
    assert_eq!(
        poll(&mut b, &w).unwrap_err().message,
        "Download guid-1 was canceled."
    );
    p.state.lock().unwrap().status = "completed".into();
    let next = arm(&mut b, json!(1000));
    fire(&mut b).unwrap();
    assert_eq!(poll(&mut b, &next).unwrap()["value"]["guid"], "guid-2");
    drop(b);
}
#[test]
fn download_actual_destination_requires_approval_before_continue() {
    let p = Provider::new();
    p.state.lock().unwrap().url = Some("http://unapproved.test/file".into());
    let mut b = p.browser();
    let w = arm(&mut b, json!(1000));
    fire(&mut b).unwrap();
    assert!(poll(&mut b, &w).is_err());
    assert!(p.calls("Fetch.continueResponse").is_empty());
    assert_eq!(p.calls("Fetch.failRequest").len(), 1);
    drop(b);
}
#[test]
fn download_source_change_during_admission_fails_before_continue() {
    let p = Provider::new();
    p.state.lock().unwrap().source_changed = true;
    let mut b = p.browser();
    let w = arm(&mut b, json!(1000));
    fire(&mut b).unwrap();
    assert_eq!(
        poll(&mut b, &w).unwrap_err().message,
        "Download source document changed during approval"
    );
    assert!(p.calls("Fetch.continueResponse").is_empty());
    drop(b);
}
#[test]
fn download_started_clears_completion_deadline_until_host_cancel() {
    let p = Provider::new();
    p.state.lock().unwrap().status = String::new();
    let mut b = p.browser();
    b.begin_chooser_cell("scope", 1);
    let w = arm(&mut b, json!(100));
    fire(&mut b).unwrap();
    thread::sleep(Duration::from_millis(110));
    assert_eq!(poll(&mut b, &w).unwrap()["pending"], true);
    b.finish_chooser_cell("scope", 1, true);
    assert!(p.calls("Fetch.disable").is_empty());
    assert!(p.calls("Browser.cancelDownload").is_empty());
    drop(b);
}
#[test]
fn download_cell_ownership_and_session_end_revoke_paths_without_deleting_files() {
    let p = Provider::new();
    p.state.lock().unwrap().filename = Some("/owned/retained.bin".into());
    let mut b = p.browser();
    b.begin_chooser_cell("one", 1);
    let w = arm(&mut b, json!(1000));
    fire(&mut b).unwrap();
    poll(&mut b, &w).unwrap();
    b.begin_chooser_cell("two", 2);
    assert_eq!(
        b.execute("download_path", &json!({"tab":"t","downloadId":"guid-1"}))
            .unwrap(),
        json!({"path":null})
    );
    b.begin_chooser_cell("one", 3);
    assert_eq!(
        b.execute("download_path", &json!({"tab":"t","downloadId":"guid-1"}))
            .unwrap()["path"],
        "/owned/retained.bin"
    );
    b.end_session();
    assert_eq!(
        b.execute("download_path", &json!({"tab":"t","downloadId":"guid-1"}))
            .unwrap(),
        json!({"path":null})
    );
    drop(b);
}
#[test]
fn download_lost_continue_ack_never_replays_request() {
    let p = Provider::new();
    p.state.lock().unwrap().drop_continue = true;
    let mut b = p.browser();
    let w = arm(&mut b, json!(1000));
    assert!(fire(&mut b).is_err());
    assert!(poll(&mut b, &w).is_err());
    assert_eq!(p.calls("Fetch.continueResponse").len(), 1);
    drop(b);
}
#[test]
fn download_successful_low_level_cell_releases_pending_watch_but_preserves_guard() {
    let p = Provider::new();
    let mut b = p.browser();
    b.begin_chooser_cell("one", 1);
    let _w = arm(&mut b, json!(120000));
    b.finish_chooser_cell("one", 1, false);
    assert!(p.calls("Fetch.disable").is_empty());
    b.begin_chooser_cell("one", 2);
    arm(&mut b, json!(1000));
    assert_eq!(p.calls("Fetch.enable").len(), 1);
    drop(b);
}

#[test]
fn download_same_process_child_binds_started_frame_to_paused_response() {
    let p = Provider::new();
    p.state.lock().unwrap().frame = Some("child-frame".into());
    let mut b = p.browser();
    let w = arm(&mut b, json!(1000));
    fire(&mut b).unwrap();
    assert_eq!(poll(&mut b, &w).unwrap()["value"]["guid"], "guid-1");
    drop(b);
}
#[test]
fn download_deferred_error_preserves_original_cdp_fallback_message() {
    let p = Provider::new();
    p.state.lock().unwrap().outer_error = Some(json!({"code":-777,"data":"owned-error"}));
    let mut b = p.browser();
    let w = arm(&mut b, json!(1000));
    let error = fire(&mut b).unwrap_err();
    assert_eq!(error.code, -777);
    assert_eq!(
        error.message,
        json!({"code":-777,"data":"owned-error"}).to_string()
    );
    poll(&mut b, &w).unwrap();
    drop(b);
}
#[test]
fn download_inherited_deadline_never_dispatches_continuation_after_slow_metadata() {
    let p = Provider::new();
    p.state.lock().unwrap().metadata_delay_ms = 50;
    let mut b = p.browser();
    let w = arm(&mut b, json!(1000));
    let _ = b.execute(
        "cdp_call",
        &json!({"tab":"t","method":"Owned.fire","params":{"timeout":20}}),
    );
    assert!(poll(&mut b, &w).is_err());
    assert!(p.calls("Fetch.continueResponse").is_empty());
    assert_eq!(p.calls("Fetch.failRequest").len(), 1);
    drop(b);
}

#[test]
fn controlled_root_blocks_unarmed_download_and_preserves_interceptor_between_waits() {
    let p = Provider::new();
    let mut b = p.browser();
    assert_eq!(fire(&mut b).unwrap()["outer"], "acknowledged");
    assert_eq!(p.calls("Fetch.failRequest").len(), 1);
    assert!(p.calls("Fetch.continueResponse").is_empty());
    let w = arm(&mut b, json!(1000));
    fire(&mut b).unwrap();
    poll(&mut b, &w).unwrap();
    assert!(p.calls("Fetch.disable").is_empty());
    fire(&mut b).unwrap();
    assert_eq!(p.calls("Fetch.failRequest").len(), 2);
    assert_eq!(p.calls("Fetch.enable").len(), 1);
    assert!(b.end_session().is_empty());
    assert_eq!(p.calls("Fetch.disable").len(), 1);
    drop(b);
}
#[test]
fn raw_fetch_configuration_cannot_steal_owned_document_responses() {
    let p = Provider::new();
    let mut b = p.browser();
    for method in [
        "Fetch.enable",
        "Fetch.disable",
        "Fetch.continueResponse",
        "Fetch.failRequest",
    ] {
        let error = b
            .execute("cdp_call", &json!({"tab":"t","method":method,"params":{}}))
            .unwrap_err();
        assert!(error.message.contains("conflicts"));
    }
    assert_eq!(p.calls("Fetch.enable").len(), 1);
    assert!(p.calls("Fetch.disable").is_empty());
    drop(b);
}

#[test]
fn download_failed_guard_removal_remains_tracked_for_acknowledged_retry() {
    let p = Provider::new();
    let mut b = p.browser();
    b.execute("get_tab", &json!({"tab":"t"})).unwrap();
    // get_tab may be metadata-only; explicit root command acquires interception.
    b.execute(
        "cdp_call",
        &json!({"tab":"t","method":"Page.enable","params":{}}),
    )
    .unwrap();
    p.state.lock().unwrap().fail_disable = true;
    let errors = b.end_session();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].1.message, "Owned disable refusal");
    assert!(b.end_session().is_empty());
    assert_eq!(p.calls("Fetch.disable").len(), 2);
    drop(b);
    drop(p);
}

#[test]
fn late_document_continuation_during_disable_preserves_the_actual_acknowledgement() {
    for refusal in [false, true] {
        let p = Provider::new();
        let mut b = p.browser();
        b.execute(
            "cdp_call",
            &json!({"tab":"t","method":"Page.enable","params":{}}),
        )
        .unwrap();
        {
            let mut state = p.state.lock().unwrap();
            state.paused_during_disable = true;
            state.fail_disable = refusal;
        }
        let errors = b.end_session();
        if refusal {
            assert_eq!(errors.len(), 1);
            assert_eq!(errors[0].1.message, "Owned disable refusal");
        } else {
            assert!(errors.is_empty(), "{errors:?}");
        }
        assert_eq!(p.calls("Fetch.continueResponse").len(), 1);
        assert!(b.end_session().is_empty());
        assert_eq!(p.calls("Fetch.disable").len(), if refusal { 2 } else { 1 });
        drop(b);
    }
}

#[test]
fn an_inactive_fetch_domain_outside_teardown_remains_an_error() {
    let p = Provider::new();
    let mut b = p.browser();
    {
        let mut state = p.state.lock().unwrap();
        state.html_response = true;
        state.inactive_continue = true;
    }
    let error = fire(&mut b).unwrap_err();
    assert_eq!(error.code, -32000);
    assert_eq!(error.message, "Fetch domain is not enabled");
    assert!(p.calls("Fetch.disable").is_empty());
    drop(b);
}

#[test]
fn navigation_download_denial_surfaces_original_error_without_sensitive_url_parts() {
    let p = Provider::new();
    let mut b = p.browser();
    let url = "https://user:secret@owned.test/file?token=private#fragment";
    p.state.lock().unwrap().navigation_error = Some(format!("net::ERR_BLOCKED_BY_CLIENT {url}"));
    let error = b
        .execute("navigate", &json!({"tab":"t","url":url}))
        .unwrap_err();
    assert_eq!(
        error.message,
        "Browser Use cannot open https://owned.test/file in tab t. Browser reported: net::ERR_BLOCKED_BY_CLIENT https://owned.test/file"
    );
    let raw = b
        .execute(
            "cdp_call",
            &json!({"tab":"t","method":"Page.navigate","params":{"url":url}}),
        )
        .unwrap();
    assert!(raw["errorText"].as_str().unwrap().contains("token=private"));
    drop(b);
    drop(p);
}

#[test]
fn new_tab_attaches_response_guard_before_initial_navigation_and_closes_only_failed_owned_target() {
    let p = Provider::new();
    let mut b = p.browser();
    p.state.lock().unwrap().navigation_error = Some("net::ERR_BLOCKED_BY_CLIENT".into());
    let error = b
        .execute("new_tab", &json!({"url":"https://owned.test/download"}))
        .unwrap_err();
    assert!(error.message.contains("net::ERR_BLOCKED_BY_CLIENT"));
    let calls = p.state.lock().unwrap().requests.clone();
    assert_eq!(calls[0]["method"], "Target.createTarget");
    assert_eq!(calls[0]["params"]["url"], "about:blank");
    let guard = calls
        .iter()
        .position(|r| r["method"] == "Fetch.enable")
        .unwrap();
    let navigation = calls
        .iter()
        .position(|r| r["method"] == "Page.navigate")
        .unwrap();
    assert!(guard < navigation);
    assert_eq!(
        calls[navigation]["params"]["url"],
        "https://owned.test/download"
    );
    assert_eq!(p.calls("Page.navigate").len(), 1);
    assert_eq!(p.calls("Target.closeTarget")[0]["params"]["targetId"], "t");
    drop(b);
    drop(p);
}

#[test]
fn post_ack_download_defers_prompt_to_owned_poll_and_revalidates_source_or_cancellation() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    struct Approval {
        ready: AtomicBool,
        prompts: AtomicUsize,
        revoked: AtomicBool,
    }
    impl skyre::security::DownloadApproval for Approval {
        fn ready_to_prompt(&self) -> skyre::Result<bool> {
            if self.revoked.load(Ordering::SeqCst) {
                return Err(skyre::Error::action("Owned approval cell revoked"));
            }
            Ok(self.ready.load(Ordering::SeqCst))
        }
        fn request(&self, _: Value, _: Option<std::time::Instant>) -> skyre::Result<Value> {
            assert!(self.ready.load(Ordering::SeqCst));
            self.prompts.fetch_add(1, Ordering::SeqCst);
            Ok(json!({"action":"accept"}))
        }
    }
    for mode in [
        "allow",
        "source_changed",
        "cancel",
        "foreign_owner",
        "revoked",
    ] {
        let provider = Provider::new();
        let mut browsers = provider.browser();
        let approval = Arc::new(Approval {
            ready: AtomicBool::new(false),
            prompts: AtomicUsize::new(0),
            revoked: AtomicBool::new(false),
        });
        let mut policy = Security::default();
        policy.set_download_approval(Some(approval.clone()));
        browsers.set_download_security(policy);
        browsers.begin_chooser_cell("owner", 1);
        let watch = arm(&mut browsers, json!(30));
        browsers
            .execute(
                "cdp_call",
                &json!({"tab":"t","method":"Owned.fire_after_ack","params":{}}),
            )
            .unwrap();
        browsers.tick_choosers();
        assert_eq!(approval.prompts.load(Ordering::SeqCst), 0);
        assert!(provider.calls("Fetch.continueResponse").is_empty());
        // Arrival timeout no longer runs while a retained response awaits an owned admission call.
        thread::sleep(Duration::from_millis(40));
        browsers.tick_choosers();
        assert!(provider.calls("Fetch.failRequest").is_empty());
        if mode == "source_changed" {
            provider.state.lock().unwrap().source_changed = true;
        }
        if mode == "cancel" {
            browsers.reset_chooser_scope("owner");
        }
        if mode == "foreign_owner" {
            browsers.begin_chooser_cell("other", 2);
            assert!(poll(&mut browsers, &watch).is_err());
            assert_eq!(approval.prompts.load(Ordering::SeqCst), 0);
            browsers.begin_chooser_cell("owner", 1);
        }
        approval.ready.store(true, Ordering::SeqCst);
        approval.revoked.store(mode == "revoked", Ordering::SeqCst);
        let result = poll(&mut browsers, &watch);
        if mode == "allow" || mode == "foreign_owner" {
            assert_eq!(result.unwrap()["value"]["guid"], "guid-1");
            assert_eq!(approval.prompts.load(Ordering::SeqCst), 1);
            assert_eq!(provider.calls("Fetch.continueResponse").len(), 1);
        } else {
            assert!(result.is_err());
            assert_eq!(approval.prompts.load(Ordering::SeqCst), 0);
            assert_eq!(provider.calls("Fetch.failRequest").len(), 1);
            assert!(provider.calls("Fetch.continueResponse").is_empty());
        }
    }
}

#[derive(Clone, Copy)]
enum HtmlClose {
    Armed,
    AwaitingContinuation,
    Closed,
}

#[test]
fn public_close_retains_actual_ack_after_html_session_retirement() {
    let mut provider = Provider::new();
    let mut browsers = provider.browser();
    let setup = browsers.execute(
        "cdp_call",
        &json!({"tab":"t","method":"Page.enable","params":{}}),
    );
    let result = setup.and_then(|_| {
        provider.state.lock().unwrap().html_close = Some(HtmlClose::Armed);
        browsers.execute("close_tab", &json!({"tab":"t"}))
    });
    let cleanup = result.is_ok().then(|| browsers.end_session());
    drop(browsers);
    // Retain a provider-thread failure before any client-result assertion,
    // without a second join panic from Provider::drop during unwinding.
    provider.join.take().unwrap().join().unwrap();
    assert_eq!(
        result.unwrap(),
        json!({"success":true,"owned":"actual-public-close-ack"})
    );
    assert!(cleanup.unwrap().is_empty());
    let state = provider.state.lock().unwrap();
    assert!(matches!(state.html_close, Some(HtmlClose::Closed)));
    let close = state
        .requests
        .iter()
        .position(|request| request["method"] == "Target.closeTarget")
        .unwrap();
    let tail = &state.requests[close..];
    assert_eq!(
        tail.iter()
            .map(|request| request["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "Target.closeTarget",
            "Fetch.continueResponse",
            "Target.getTargets",
        ]
    );
    assert_eq!(tail[0]["params"], json!({"targetId":"t"}));
    assert_eq!(tail[1]["sessionId"], "root");
    assert_eq!(
        tail[1]["params"],
        json!({"requestId":"late-close-document"})
    );
    assert_eq!(
        state
            .requests
            .iter()
            .filter(|request| request["method"] == "Target.attachToTarget")
            .count(),
        1
    );
    assert_eq!(
        state
            .requests
            .iter()
            .filter(|request| request["method"] == "Fetch.enable")
            .count(),
        1
    );
    assert!(
        state
            .requests
            .iter()
            .all(|request| request["method"] != "Fetch.disable")
    );
}
