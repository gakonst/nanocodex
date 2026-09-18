//! Scheduler tests use production admission/poll/Wire and live worker controls.
//! Only the native child is replaced; no desktop UI or timing-based ordering.
use super::*;
use serde_json::json;
use skyre::{
    fixture::Fixture,
    runtime::{HostOptions, RuntimeBackend},
    worker::{Event, Worker},
};
use std::path::{Path, PathBuf};

const BOUND: Duration = Duration::from_secs(10);

struct Harness {
    lanes: Lanes,
    engine: Engine,
    control: ProviderControl,
    // Retain the real provider reply so its capability remains live.
    _reply: SyncSender<Result<Value>>,
    _worker: Worker,
    dir: tempfile::TempDir,
}
impl Harness {
    fn new() -> Self {
        let mut worker = Worker::with_options(HostOptions {
            runtime: RuntimeBackend::Quickjs,
            ..Default::default()
        });
        worker
            .start(
                "await agent.browsers.get('lane-test-held')",
                Duration::from_secs(60),
            )
            .unwrap();
        let (control, reply) = loop {
            match worker
                .event(BOUND)
                .unwrap()
                .expect("worker provider within bound")
            {
                Event::Call { method, reply, .. } if method == "sky.setup" => {
                    reply.send(Ok(json!({"target":"mac"}))).unwrap()
                }
                Event::Call {
                    method,
                    reply,
                    control,
                    ..
                } => {
                    assert_eq!(method, "browser.info");
                    break (control, reply);
                }
                Event::Done { result, .. } => panic!("worker finished before provider: {result:?}"),
            }
        };
        let mut engine = Engine::new(Box::new(Fixture::default()));
        let policy = engine
            .execute("sky.app_policy", &json!({"app":"fixture://native"}))
            .unwrap();
        let (_, approval) = engine.prepare_elicitation(&json!({"meta":{"connector_id":"computer-use","tool_name":"get_app_state","tool_params":{"app":policy["target"]["bundleIdentifier"]}}})).unwrap();
        assert_eq!(approval.unwrap()["action"], "accept");
        Self {
            lanes: Lanes::default(),
            engine,
            control,
            _reply: reply,
            _worker: worker,
            dir: tempfile::tempdir().unwrap(),
        }
    }
    fn path(&self, window: u32, suffix: &str) -> PathBuf {
        self.dir.path().join(format!("{window}.{suffix}"))
    }
    fn call(&self, window: u32, id: u32, hold: bool, lost: bool) -> WindowLaneCall {
        WindowLaneCall {
            key: format!("window-{window}"),
            scope: "owned-scope".into(),
            app: native::App {
                window_id: Some(window),
                pid: 4242,
                id: "fixture".into(),
                name: "Fixture".into(),
                path: "fixture://native".into(),
            },
            method: "get_app_state".into(),
            params: json!({"id":id,"hold":hold,"lost":lost}),
            lease: None,
        }
    }
    fn insert(&mut self, window: u32) {
        let call = self.call(window, 0, false, false);
        let mut command = Command::new("/usr/bin/python3");
        command
            .args([
                "-u",
                "-c",
                r#"
import json,sys,os,time,threading,queue
binding=json.loads(sys.stdin.readline())
base=sys.argv[1]
requests=queue.Queue()
captured=queue.Queue()
def read():
    for line in sys.stdin:
        wire=json.loads(line)
        assert wire['kind'] in ('call','renew','captured')
        if wire['kind']=='renew':
            with open(base+'.renew','a') as log: log.write('renew\n')
        elif wire['kind']=='captured':
            captured.put(wire['call'])
        else:
            assert set(wire)=={'kind','call'}
            requests.put(wire['call'])
    os._exit(0)
threading.Thread(target=read,daemon=True).start()
while True:
    call=requests.get()
    assert call['method']=='get_app_state'
    p=call['params']
    with open(base+'.calls','a') as log: log.write(str(p['id'])+'\n')
    if p['lost']: os._exit(0)
    if p.get('capture'):
        print(json.dumps({'kind':'capture','request':{'pid':binding['app']['pid'],'window_id':binding['app']['window_id'],'frame':[0,0,100,100],'pixels':[100,100],'encoding':'Png'}}),flush=True)
        reply=captured.get(timeout=10)
        assert reply['kind']=='error'
        print(json.dumps({'kind':'ok','value':{'id':p['id'],'pid':os.getpid(),'text':'owned AX','screenshotError':reply['message']}}),flush=True)
        continue
    if p['hold']:
        open(base+'.entered','w').close()
        deadline=time.monotonic()+15
        while not os.path.exists(base+'.release'):
            if time.monotonic()>deadline: os._exit(2)
            time.sleep(.002)
    print(json.dumps({'kind':'ok','value':{'id':p['id'],'pid':os.getpid()}}),flush=True)
"#,
            ])
            .arg(self.dir.path().join(window.to_string()))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let process = Process::with_command(
            command,
            &Bootstrap {
                version: 1,
                app: call.app.clone(),
                scope: call.scope.clone(),
            },
        )
        .unwrap();
        self.lanes.lanes.insert(
            (call.scope, call.key),
            Lane {
                capture: None,
                renewed: Instant::now(),
                process,
                active: None,
                queue: VecDeque::new(),
            },
        );
    }
    fn submit(&mut self, window: u32, id: u32, hold: bool, lost: bool) -> Receiver<Result<Value>> {
        let (send, receive) = mpsc::sync_channel(1);
        self.lanes
            .submit(
                self.call(window, id, hold, lost),
                self.control.clone(),
                send,
            )
            .unwrap();
        receive
    }
    fn until(&mut self, mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + BOUND;
        while !ready() {
            assert!(
                Instant::now() < deadline,
                "scheduler did not progress within bound"
            );
            self.lanes.poll(&mut self.engine);
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn receive(&mut self, receiver: &Receiver<Result<Value>>) -> Result<Value> {
        let mut result = None;
        self.until(|| match receiver.try_recv() {
            Ok(value) => {
                result = Some(value);
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(error) => panic!("reply disconnected: {error}"),
        });
        result.unwrap()
    }
    fn receipt(&mut self, path: &Path) {
        self.until(|| path.exists());
    }
    fn calls(&self, window: u32) -> String {
        std::fs::read_to_string(self.path(window, "calls")).unwrap_or_default()
    }
}

#[test]
fn scheduler_causal_other_window_completes_before_release_and_same_window_is_fifo() {
    let mut h = Harness::new();
    h.insert(1);
    h.insert(2);
    let first = h.submit(1, 1, true, false);
    let entered = h.path(1, "entered");
    h.receipt(&entered);
    let second = h.submit(1, 2, false, false);
    let other = h.submit(2, 3, false, false);
    // Force the production renewal branch deterministically, without waiting
    // for wall-clock passage to establish any causal ordering.
    h.lanes
        .lanes
        .get_mut(&("owned-scope".into(), "window-1".into()))
        .unwrap()
        .renewed = Instant::now() - Duration::from_secs(1);
    let b = h.receive(&other).unwrap();
    assert_eq!(b["id"], 3);
    let renewed = h.path(1, "renew");
    h.receipt(&renewed);
    assert!(!h.path(1, "release").exists());
    assert!(matches!(first.try_recv(), Err(TryRecvError::Empty)));
    assert!(matches!(second.try_recv(), Err(TryRecvError::Empty)));
    assert_eq!(h.calls(1), "1\n");
    std::fs::write(h.path(1, "release"), b"release").unwrap();
    let a = h.receive(&first).unwrap();
    let next = h.receive(&second).unwrap();
    assert_eq!(a["id"], 1);
    assert_eq!(next["id"], 2);
    assert_eq!(a["pid"], next["pid"], "same window retains worker");
    assert_eq!(h.calls(1), "1\n2\n");
}

#[test]
fn scheduler_scope_reset_rejects_active_and_queued_without_execution() {
    let mut h = Harness::new();
    h.insert(1);
    let active = h.submit(1, 1, true, false);
    let entered = h.path(1, "entered");
    h.receipt(&entered);
    let queued = h.submit(1, 2, false, false);
    h.lanes.reset_scope("owned-scope");
    assert_eq!(
        active.recv_timeout(BOUND).unwrap().unwrap_err().code,
        -32800
    );
    assert_eq!(
        queued.recv_timeout(BOUND).unwrap().unwrap_err().code,
        -32800
    );
    assert!(h.lanes.lanes.is_empty());
    h.lanes.poll(&mut h.engine);
    assert_eq!(h.calls(1), "1\n");
    assert!(!h.path(1, "release").exists());
}

#[test]
fn scheduler_lost_receipt_fails_queue_without_retry() {
    let mut h = Harness::new();
    h.insert(1);
    let lost = h.submit(1, 1, false, true);
    let queued = h.submit(1, 2, false, false);
    assert!(h.receive(&lost).is_err());
    assert!(h.receive(&queued).is_err());
    assert!(h.lanes.lanes.is_empty());
    h.lanes.poll(&mut h.engine);
    assert_eq!(
        h.calls(1),
        "1\n",
        "delivered request must never retry after lost receipt"
    );
}

#[test]
fn scheduler_admission_bounds_reject_without_dispatch() {
    let mut h = Harness::new();
    h.insert(1);
    let mut replies = Vec::new();
    for id in 0..MAX_QUEUE {
        replies.push(h.submit(1, id as u32, false, false));
    }
    let (send, _receive) = mpsc::sync_channel(1);
    let error = h
        .lanes
        .submit(h.call(1, 999, false, false), h.control.clone(), send)
        .unwrap_err();
    assert!(error.message.contains("queue full"));
    let mut large = h.call(1, 1000, false, false);
    large.params = json!({"payload":"x".repeat(MAX_REQUEST)});
    let (send, _receive) = mpsc::sync_channel(1);
    assert!(
        h.lanes
            .submit(large, h.control.clone(), send)
            .unwrap_err()
            .message
            .contains("byte limit")
    );
    assert_eq!(h.calls(1), "");
    h.lanes.reset_scope("owned-scope");
    for reply in replies {
        assert_eq!(reply.recv_timeout(BOUND).unwrap().unwrap_err().code, -32800);
    }
    assert_eq!(h.calls(1), "");
}

#[test]
fn capture_wire_is_scalar_bound_and_does_not_complete_active_call() {
    let h = Harness::new();
    let call = h.call(7, 1, false, false);
    let request = native::WindowCapture {
        pid: 4242,
        window_id: Some(7),
        frame: [10., 20., 400., 300.],
        pixels: [400, 300],
        encoding: native::screenshot::Encoding::Png,
    };
    let frame = encode(&Response::Capture {
        request: request.clone(),
    })
    .unwrap();
    let Response::Capture { request: decoded } = serde_json::from_slice(&frame).unwrap() else {
        panic!("capture must not decode as completion")
    };
    validate_capture(&call, &decoded, false).unwrap();
    assert!(validate_capture(&call, &decoded, true).is_err());
    let mut wrong = request.clone();
    wrong.pid += 1;
    assert!(validate_capture(&call, &wrong, false).is_err());
    wrong = request.clone();
    wrong.window_id = None;
    assert!(validate_capture(&call, &wrong, false).is_err());
    wrong.window_id = Some(8);
    assert!(validate_capture(&call, &wrong, false).is_err());
    wrong = request.clone();
    wrong.pixels = [2048, 2048];
    assert!(validate_capture(&call, &wrong, false).is_err());
    let mut input = h.call(7, 2, false, false);
    input.method = "click".into();
    assert!(validate_capture(&input, &request, false).is_err());
    let wire = encode(&Wire::Captured(CaptureReply::from_result(Ok(
        native::Image {
            mime_type: "image/png".into(),
            data: "owned-pixels".into(),
        },
    ))))
    .unwrap();
    let Wire::Captured(reply) = serde_json::from_slice(&wire).unwrap() else {
        panic!("capture reply must stay separate from call queue")
    };
    assert_eq!(reply.result().unwrap().data, "owned-pixels");
    let wire = encode(&Wire::Captured(CaptureReply::from_result(Err(
        Error::action("Screenshot timed out"),
    ))))
    .unwrap();
    let Wire::Captured(reply) = serde_json::from_slice(&wire).unwrap() else {
        panic!()
    };
    assert_eq!(reply.result().unwrap_err().message, "Screenshot timed out");
}

#[test]
fn pending_capture_keeps_authority_renewing_and_reset_discards_receiver() {
    let mut h = Harness::new();
    h.insert(1);
    h.insert(2);
    let active = h.submit(1, 1, true, false);
    let entered = h.path(1, "entered");
    h.receipt(&entered);
    let (capture_send, receive) = mpsc::channel();
    let lane = h
        .lanes
        .lanes
        .get_mut(&("owned-scope".into(), "window-1".into()))
        .unwrap();
    lane.capture = Some(Capture {
        started: Instant::now(),
        receive,
    });
    lane.renewed = Instant::now() - Duration::from_secs(1);
    let other = h.submit(2, 2, false, false);
    assert_eq!(h.receive(&other).unwrap()["id"], 2);
    let renewed = h.path(1, "renew");
    h.receipt(&renewed);
    assert!(matches!(active.try_recv(), Err(TryRecvError::Empty)));
    h.lanes.reset_scope("owned-scope");
    assert_eq!(
        active.recv_timeout(BOUND).unwrap().unwrap_err().code,
        -32800
    );
    assert!(
        capture_send
            .send(Err(Error::action("late capture")))
            .is_err()
    );
}

#[test]
fn ordinary_capture_start_failure_preserves_optional_result_and_queue() {
    let mut h = Harness::new();
    h.insert(1);
    let mut call = h.call(1, 1, false, false);
    call.params["capture"] = json!(true);
    let (send, first) = mpsc::sync_channel(1);
    h.lanes.submit(call, h.control.clone(), send).unwrap();
    let second = h.submit(1, 2, false, false);
    let mut started = false;
    let deadline = Instant::now() + BOUND;
    while !started {
        assert!(Instant::now() < deadline);
        h.lanes.poll_with_capture(&mut h.engine, |_| {
            started = true;
            Err(Error::new(-32003, "Screen Recording denied"))
        });
        std::thread::sleep(Duration::from_millis(2));
    }
    let result = h.receive(&first).unwrap();
    assert_eq!(result["text"], "owned AX");
    assert_eq!(result["screenshotError"], "Screen Recording denied");
    let next = h.receive(&second).unwrap();
    assert_eq!(next["id"], 2);
    assert_eq!(result["pid"], next["pid"]);
}

#[test]
fn active_lane_renews_and_completes_before_pending_elicitation_is_answered() {
    let mut h = Harness::new();
    h.insert(1);
    let reply = h.submit(1, 1, true, false);
    let entered = h.path(1, "entered");
    h.receipt(&entered);
    let mut server = crate::elicitation_tests::server();
    server.native_lanes = std::mem::take(&mut h.lanes);
    server.engine = std::rc::Rc::new(std::cell::RefCell::new(std::mem::replace(
        &mut h.engine,
        Engine::new(Box::new(Fixture::default())),
    )));
    let (send, receive) = mpsc::channel();
    server.input = Some(receive);
    server.host_options.elicitation_timeout_ms = Some(5000);
    let renewed = h.path(1, "renew");
    let release = h.path(1, "release");
    let mut reply = Some(reply);
    let mut thread = None;
    let result = server.await_elicitation(
        &json!({"message":"owned pending approval"}),
        &mut |outgoing| {
            let id = outgoing["id"].clone();
            let send = send.clone();
            let renewed = renewed.clone();
            let release = release.clone();
            let reply = reply.take().unwrap();
            thread = Some(std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(350));
                let renewals = std::fs::read_to_string(renewed).unwrap_or_default();
                assert!(renewals.lines().count() >= 3, "approval blocked renewal");
                std::fs::write(release, b"release").unwrap();
                assert_eq!(reply.recv_timeout(BOUND).unwrap().unwrap()["id"], 1);
                send.send(Ok(Some(
                    json!({"jsonrpc":"2.0","id":id,"result":{"action":"decline"}}),
                )))
                .unwrap();
            }));
            Ok(())
        },
    );
    thread.unwrap().join().unwrap();
    assert_eq!(result.unwrap()["action"], "decline");
}
