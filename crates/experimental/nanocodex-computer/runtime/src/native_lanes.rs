//! Persistent, exact-window native workers. Each worker owns AppKit on its main
//! thread. Parent admission and completion run on the companion's main loop;
//! pipe I/O never holds the Engine or blocks unrelated windows.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use skyre::{
    Error, Result,
    engine::{Engine, WindowLaneCall},
    native,
    runtime::ProviderControl,
};
use std::{
    collections::{BTreeMap, VecDeque},
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
    time::{Duration, Instant},
};

pub const CHILD_ARGUMENT: &str = "__native-window-lane";
const MAX_LANES: usize = 32;
const MAX_QUEUE: usize = 32;
const MAX_PENDING: usize = 256;
const MAX_REQUEST: usize = 2 * 1024 * 1024;
const MAX_PENDING_BYTES: usize = 16 * 1024 * 1024;
const MAX_FRAME: usize = 32 * 1024 * 1024;
// Include draining/revoked processes: repeated cancellation cannot accumulate
// unlimited native children even if an OS capture never returns.
static PROCESSES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Bootstrap {
    version: u8,
    app: native::App,
    scope: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    method: String,
    params: Value,
}
#[derive(Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "call",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Wire {
    Call(Request),
    Renew,
    Captured(CaptureReply),
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum CaptureReply {
    Ok { image: native::Image },
    Error { code: i32, message: String },
}
impl CaptureReply {
    fn from_result(result: Result<native::Image>) -> Self {
        match result {
            Ok(image) => Self::Ok { image },
            Err(error) => Self::Error {
                code: error.code,
                message: error.message,
            },
        }
    }
    fn result(self) -> Result<native::Image> {
        match self {
            Self::Ok { image } => Ok(image),
            Self::Error { code, message } => Err(Error::new(code, message)),
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Response {
    Capture { request: native::WindowCapture },
    Ok { value: Value },
    Error { code: i32, message: String },
}
impl Response {
    fn from_result(result: Result<Value>) -> Self {
        match result {
            Ok(value) => Self::Ok { value },
            Err(error) => Self::Error {
                code: error.code,
                message: error.message,
            },
        }
    }
    fn result(self) -> Result<Value> {
        match self {
            Self::Capture { .. } => Err(Error::action("Unexpected capture response")),
            Self::Ok { value } => Ok(value),
            Self::Error { code, message } => Err(Error::new(code, message)),
        }
    }
}
fn read_frame(reader: &mut impl BufRead) -> Result<Option<Vec<u8>>> {
    let mut frame = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if frame.is_empty() {
                Ok(None)
            } else {
                Err(Error::action("Truncated native lane frame"))
            };
        }
        let count = available
            .iter()
            .position(|b| *b == b'\n')
            .map_or(available.len(), |n| n + 1);
        if frame.len() + count > MAX_FRAME {
            return Err(Error::action("Native lane frame limit exceeded"));
        }
        let done = available[count - 1] == b'\n';
        frame.extend_from_slice(&available[..count]);
        reader.consume(count);
        if done {
            return Ok(Some(frame));
        }
    }
}
fn encode(value: &impl Serialize) -> Result<Vec<u8>> {
    let mut frame = serde_json::to_vec(value)?;
    if frame.len() >= MAX_FRAME {
        return Err(Error::invalid("Native lane frame limit exceeded"));
    }
    frame.push(b'\n');
    Ok(frame)
}
struct Process {
    safe_to_kill: bool,
    child: Option<Child>,
    send: Option<SyncSender<Vec<u8>>>,
    receive: Receiver<Result<Response>>,
}
impl Process {
    fn spawn(bootstrap: &Bootstrap) -> Result<Self> {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg(CHILD_ARGUMENT)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        command.envs(native::window_lane_environment()?);
        Self::with_command(command, bootstrap)
    }
    fn with_command(mut command: Command, bootstrap: &Bootstrap) -> Result<Self> {
        use std::sync::atomic::Ordering;
        if PROCESSES
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_LANES).then_some(count + 1)
            })
            .is_err()
        {
            return Err(Error::action(
                "Native worker process limit reached (including draining workers)",
            ));
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                PROCESSES.fetch_sub(1, Ordering::AcqRel);
                return Err(error.into());
            }
        };
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::action("Native lane stdin missing"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::action("Native lane stdout missing"))?;
        let (send, writes) = mpsc::sync_channel::<Vec<u8>>(2);
        let (responses, receive) = mpsc::sync_channel(2);
        let errors = responses.clone();
        std::thread::spawn(move || {
            for frame in writes {
                if let Err(error) = stdin.write_all(&frame).and_then(|_| stdin.flush()) {
                    let _ = errors.send(Err(error.into()));
                    break;
                }
            }
            // EOF is a revocation signal, never another native request.
        });
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let result = read_frame(&mut reader)
                    .and_then(|frame| frame.ok_or_else(|| Error::action("Native lane exited")))
                    .and_then(|frame| serde_json::from_slice(&frame).map_err(Error::from));
                let failed = result.is_err();
                if responses.send(result).is_err() || failed {
                    break;
                }
            }
        });
        let process = Self {
            safe_to_kill: true,
            child: Some(child),
            send: Some(send),
            receive,
        };
        process.write(encode(bootstrap)?)?;
        Ok(process)
    }
    fn write(&self, frame: Vec<u8>) -> Result<()> {
        self.send
            .as_ref()
            .ok_or_else(|| Error::action("Native lane closed"))?
            .try_send(frame)
            .map_err(|_| Error::action("Native lane write queue full or closed"))
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        self.send.take();
        if let Some(mut child) = self.child.take() {
            // Idle/capture workers never own held input and can be reclaimed
            // even when an OS capture is hung indefinitely.
            if self.safe_to_kill {
                let _ = child.kill();
            }
            // Do not kill a process while it may hold native buttons/modifiers.
            // EOF revokes its receiver; cleanup drains on its own main thread.
            std::thread::spawn(move || {
                let _ = child.wait();
                PROCESSES.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
            });
        }
    }
}
struct Pending {
    bytes: usize,
    call: WindowLaneCall,
    control: ProviderControl,
    reply: SyncSender<Result<Value>>,
}
struct Capture {
    started: Instant,
    receive: Receiver<Result<native::Image>>,
}
struct Lane {
    capture: Option<Capture>,
    renewed: Instant,
    process: Process,
    active: Option<Pending>,
    queue: VecDeque<Pending>,
}
#[derive(Default)]
pub struct Lanes {
    lanes: BTreeMap<(String, String), Lane>,
}
impl Lanes {
    pub fn submit(
        &mut self,
        call: WindowLaneCall,
        control: ProviderControl,
        reply: SyncSender<Result<Value>>,
    ) -> Result<()> {
        let bytes = serde_json::to_vec(&call.params)?.len();
        let retained: usize = self
            .lanes
            .values()
            .flat_map(|lane| lane.active.iter().chain(lane.queue.iter()))
            .map(|pending| pending.bytes)
            .sum();
        if bytes > MAX_REQUEST || retained.saturating_add(bytes) > MAX_PENDING_BYTES {
            return Err(Error::invalid("Native pending request byte limit reached"));
        }
        let key = (call.scope.clone(), call.key.clone());
        if self
            .lanes
            .values()
            .map(|l| l.queue.len() + usize::from(l.active.is_some()))
            .sum::<usize>()
            >= MAX_PENDING
        {
            return Err(Error::action("Native lane pending limit reached"));
        }
        if !self.lanes.contains_key(&key) {
            if self.lanes.len() >= MAX_LANES {
                return Err(Error::action("Native window lane limit reached"));
            }
            let process = Process::spawn(&Bootstrap {
                version: 1,
                app: call.app.clone(),
                scope: call.scope.clone(),
            })?;
            self.lanes.insert(
                key.clone(),
                Lane {
                    capture: None,
                    renewed: Instant::now(),
                    process,
                    active: None,
                    queue: VecDeque::new(),
                },
            );
        }
        let lane = self.lanes.get_mut(&key).unwrap();
        if lane.queue.len() >= MAX_QUEUE {
            return Err(Error::action("Native window queue full"));
        }
        lane.queue.push_back(Pending {
            bytes,
            call,
            control,
            reply,
        });
        Ok(())
    }
    pub fn poll(&mut self, engine: &mut Engine) {
        self.poll_with_capture(engine, native::start_window_capture);
    }
    fn poll_with_capture(
        &mut self,
        engine: &mut Engine,
        mut start_capture: impl FnMut(native::WindowCapture) -> Result<Receiver<Result<native::Image>>>,
    ) {
        let mut failed = Vec::new();
        for (key, lane) in &mut self.lanes {
            if let Some(active) = &lane.active {
                if let Err(error) = engine.validate_window_lane(&active.call, &active.control) {
                    failed.push((key.clone(), error));
                    continue;
                }
                // An unresponsive parent cannot leave live native input authority.
                if lane.renewed.elapsed() >= Duration::from_millis(50) {
                    if let Err(error) =
                        encode(&Wire::Renew).and_then(|frame| lane.process.write(frame))
                    {
                        failed.push((key.clone(), error));
                        continue;
                    }
                    lane.renewed = Instant::now();
                }
                if let Some(capture) = &lane.capture {
                    let result = match capture.receive.try_recv() {
                        Ok(result) => Some(result),
                        Err(TryRecvError::Disconnected) => {
                            Some(Err(Error::action("Screenshot callback disconnected")))
                        }
                        Err(TryRecvError::Empty)
                            if capture.started.elapsed() >= Duration::from_secs(10) =>
                        {
                            Some(Err(Error::action("Screenshot timed out")))
                        }
                        Err(TryRecvError::Empty) => None,
                    };
                    if let Some(result) = result {
                        lane.capture.take();
                        if let Err(error) =
                            encode(&Wire::Captured(CaptureReply::from_result(result)))
                                .and_then(|frame| lane.process.write(frame))
                        {
                            failed.push((key.clone(), error));
                            continue;
                        }
                    }
                }
                match lane.process.receive.try_recv() {
                    Ok(Ok(Response::Capture { request })) => {
                        if let Err(error) =
                            validate_capture(&active.call, &request, lane.capture.is_some())
                        {
                            // Protocol violations poison the lane before any capture.
                            failed.push((key.clone(), error));
                            continue;
                        }
                        match start_capture(request) {
                            Ok(receive) => {
                                lane.capture = Some(Capture {
                                    started: Instant::now(),
                                    receive,
                                })
                            }
                            Err(error) => {
                                // Ordinary capture failures belong to the child Engine:
                                // optional screenshots preserve AX plus screenshotError.
                                if let Err(error) =
                                    encode(&Wire::Captured(CaptureReply::from_result(Err(error))))
                                        .and_then(|frame| lane.process.write(frame))
                                {
                                    failed.push((key.clone(), error));
                                    continue;
                                }
                            }
                        }
                    }
                    Ok(result) => {
                        if lane.capture.is_some() {
                            failed.push((
                                key.clone(),
                                Error::action("Native lane completed during capture"),
                            ));
                            continue;
                        }
                        let active = lane.active.take().unwrap();
                        let transport_failed = result.is_err();
                        if !transport_failed {
                            lane.process.safe_to_kill = true;
                        }
                        let result = result.and_then(Response::result);
                        let _ = active.reply.send(result);
                        if transport_failed {
                            failed.push((key.clone(), Error::action("Native lane failed")));
                            continue;
                        }
                    }
                    Err(TryRecvError::Empty) => (),
                    Err(TryRecvError::Disconnected) => {
                        failed.push((key.clone(), Error::action("Native lane disconnected")));
                        continue;
                    }
                }
            }
            if lane.active.is_none() {
                while let Some(next) = lane.queue.pop_front() {
                    if let Err(error) = engine.validate_window_lane(&next.call, &next.control) {
                        let _ = next.reply.send(Err(error));
                        continue;
                    }
                    // Mark before transport write: even a lost write receipt
                    // may already have delivered an input request to the child.
                    lane.process.safe_to_kill = next.call.method == "get_app_state";
                    let sent = encode(&Wire::Call(Request {
                        method: next.call.method.clone(),
                        params: next.call.params.clone(),
                    }))
                    .and_then(|frame| lane.process.write(frame));
                    match sent {
                        Ok(()) => {
                            lane.renewed = Instant::now();
                            lane.active = Some(next);
                            break;
                        }
                        Err(error) => {
                            let _ = next.reply.send(Err(error.clone()));
                            failed.push((key.clone(), error));
                            break;
                        }
                    }
                }
            }
        }
        for (key, error) in failed {
            self.remove(&key, error);
        }
    }
    fn remove(&mut self, key: &(String, String), error: Error) {
        if let Some(mut lane) = self.lanes.remove(key) {
            for pending in lane.active.take().into_iter().chain(lane.queue.drain(..)) {
                let _ = pending.reply.send(Err(error.clone()));
            }
        }
    }
    pub fn reset_scope(&mut self, scope: &str) {
        let keys: Vec<_> = self
            .lanes
            .keys()
            .filter(|key| key.0 == scope)
            .cloned()
            .collect();
        for key in keys {
            self.remove(&key, Error::new(-32800, "Native window scope reset"));
        }
    }
    pub fn clear(&mut self) {
        let keys: Vec<_> = self.lanes.keys().cloned().collect();
        for key in keys {
            self.remove(&key, Error::new(-32800, "Native window connection ended"));
        }
    }
}
impl Drop for Lanes {
    fn drop(&mut self) {
        self.clear();
    }
}

fn validate_capture(
    call: &WindowLaneCall,
    request: &native::WindowCapture,
    pending: bool,
) -> Result<()> {
    if pending
        || call.method != "get_app_state"
        || request.pid != call.app.pid
        || request.window_id.is_none()
        || request.window_id != call.app.window_id
    {
        return Err(Error::invalid(
            "Capture does not match active approved window",
        ));
    }
    request.validate()
}

pub fn run_child() -> Result<()> {
    let (send, receive) = mpsc::sync_channel(2);
    let (captured_send, captured_receive) = mpsc::sync_channel(1);
    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let revoke = cancelled.clone();
    let clock = Instant::now();
    let deadline = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let renewal = deadline.clone();
    std::thread::spawn(move || {
        let mut input = BufReader::new(std::io::stdin());
        let reading = (|| -> Result<()> {
            let bootstrap = read_frame(&mut input)?
                .ok_or_else(|| Error::action("Native lane bootstrap missing"))?;
            send.send(Ok(bootstrap))
                .map_err(|_| Error::action("Native lane closed"))?;
            while let Some(frame) = read_frame(&mut input)? {
                let wire: Wire = serde_json::from_slice(&frame)?;
                renewal.store(
                    clock.elapsed().as_millis().min(u64::MAX as u128) as u64 + 250,
                    std::sync::atomic::Ordering::Release,
                );
                match wire {
                    Wire::Call(request) => send
                        .try_send(encode(&request))
                        .map_err(|_| Error::action("Native lane command queue full"))?,
                    Wire::Captured(reply) => captured_send
                        .try_send(reply)
                        .map_err(|_| Error::action("Native capture reply queue full"))?,
                    Wire::Renew => (),
                }
            }
            Ok(())
        })();
        revoke.store(true, std::sync::atomic::Ordering::Release);
        if let Err(error) = reading {
            let _ = send.try_send(Err(error));
        }
    });
    native::set_native_cancellation(Some(cancelled.clone()));
    native::set_native_execution_deadline(clock, deadline);
    let bootstrap: Bootstrap = serde_json::from_slice(
        &receive
            .recv()
            .map_err(|_| Error::action("Native lane bootstrap missing"))??,
    )?;
    if bootstrap.version != 1 || bootstrap.app.pid <= 0 || bootstrap.scope.len() > 4096 {
        return Err(Error::invalid("Invalid native lane binding"));
    }
    native::set_window_capture_delegate(Some(Box::new(move |request| {
        native::check_native_cancellation()?;
        let mut output = std::io::stdout();
        output.write_all(&encode(&Response::Capture { request })?)?;
        output.flush()?;
        let start = Instant::now();
        loop {
            native::check_native_cancellation()?;
            match captured_receive.try_recv() {
                Ok(reply) => return reply.result(),
                Err(TryRecvError::Disconnected) => {
                    return Err(Error::action("Capture broker disconnected"));
                }
                Err(TryRecvError::Empty) => (),
            }
            if start.elapsed() >= Duration::from_secs(11) {
                return Err(Error::action("Capture broker timed out"));
            }
            super::pump_native_run_loop();
            std::thread::sleep(Duration::from_millis(1));
        }
    })));
    let mut engine = Engine::new(native::create()?);
    let mut output = std::io::stdout();
    let result = (|| -> Result<()> {
        loop {
            if cancelled.load(std::sync::atomic::Ordering::Acquire) {
                break;
            }
            super::pump_native_run_loop();
            match receive.recv_timeout(Duration::from_millis(16)) {
                Ok(frame) => {
                    let request: Request = serde_json::from_slice(&frame?)?;
                    let result = native::check_native_cancellation().and_then(|()| {
                        engine.execute_window_lane(
                            &bootstrap.app,
                            &bootstrap.scope,
                            &request.method,
                            &request.params,
                        )
                    });
                    output.write_all(&encode(&Response::from_result(result))?)?;
                    output.flush()?;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => (),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        Ok(())
    })();
    engine.end_session();
    native::set_window_capture_delegate(None);
    result
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Instant;
    fn bootstrap(window: u32) -> Bootstrap {
        Bootstrap {
            version: 1,
            app: native::App {
                window_id: Some(window),
                pid: 4242,
                id: "owned".into(),
                name: "Owned".into(),
                path: "/Owned.app".into(),
            },
            scope: "test".into(),
        }
    }
    fn process(window: u32, barrier: &std::path::Path) -> Process {
        let mut command = Command::new("/usr/bin/python3");
        command.args(["-u","-c",r#"
import json,sys,os,time
binding=json.loads(sys.stdin.readline())
for line in sys.stdin:
    request=json.loads(line)
    if request['method']=='hold':
        open(sys.argv[1]+'.entered','w').close()
        while not os.path.exists(sys.argv[1]): time.sleep(.005)
    print(json.dumps({'kind':'ok','value':{'window':binding['app']['window_id'],'pid':os.getpid()}}),flush=True)
"#]).arg(barrier).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        Process::with_command(command, &bootstrap(window)).unwrap()
    }
    #[test]
    fn slow_window_process_does_not_block_other_window_and_lanes_persist() {
        let directory = tempfile::tempdir().unwrap();
        let barrier = directory.path().join("release");
        // A real process blocks behind a test-owned barrier. B must respond
        // before release; elapsed-time heuristics cannot make this test pass.
        let a = process(1, &barrier);
        let b = process(2, &barrier);
        a.write(
            encode(&Request {
                method: "hold".into(),
                params: Value::Null,
            })
            .unwrap(),
        )
        .unwrap();
        let start = Instant::now();
        while !barrier.with_extension("entered").exists() {
            assert!(start.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(5));
        }
        b.write(
            encode(&Request {
                method: "capture".into(),
                params: Value::Null,
            })
            .unwrap(),
        )
        .unwrap();
        let first = b
            .receive
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap()
            .result()
            .unwrap();
        assert_eq!(first["window"], 2);
        assert!(matches!(a.receive.try_recv(), Err(TryRecvError::Empty)));
        b.write(
            encode(&Request {
                method: "input".into(),
                params: Value::Null,
            })
            .unwrap(),
        )
        .unwrap();
        let second = b
            .receive
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap()
            .result()
            .unwrap();
        assert_eq!(first["pid"], second["pid"], "window worker must persist");
        std::fs::write(barrier, b"release").unwrap();
        assert_eq!(
            a.receive
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap()
                .result()
                .unwrap()["window"],
            1
        );
    }
    #[test]
    fn null_native_input_success_round_trips() {
        let frame = encode(&Response::from_result(Ok(Value::Null))).unwrap();
        let response: Response = serde_json::from_slice(&frame).unwrap();
        assert_eq!(response.result().unwrap(), Value::Null);
    }
    #[test]
    fn oversized_or_truncated_frames_fail_closed() {
        assert!(read_frame(&mut std::io::Cursor::new(vec![b'x'; MAX_FRAME + 1])).is_err());
        assert!(read_frame(&mut std::io::Cursor::new(b"unterminated")).is_err());
        assert!(
            read_frame(&mut std::io::Cursor::new(b""))
                .unwrap()
                .is_none()
        );
        assert!(
            serde_json::from_str::<Request>(
                r#"{"method":"click","params":{},"credentials":"forbidden"}"#
            )
            .is_err()
        );
    }
    #[test]
    fn closing_transport_revokes_child_without_blocking_unrelated_process() {
        let directory = tempfile::tempdir().unwrap();
        let barrier = directory.path().join("release");
        let a = process(1, &barrier);
        let b = process(2, &barrier);
        let pid = a.child.as_ref().unwrap().id();
        drop(a);
        b.write(
            encode(&Request {
                method: "capture".into(),
                params: Value::Null,
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            b.receive
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap()
                .result()
                .unwrap()["window"],
            2
        );
        let start = Instant::now();
        while unsafe { libc::kill(pid as i32, 0) } == 0 {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "idle revoked child not reaped"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[cfg(all(test, unix))]
#[path = "native_lanes_tests.rs"]
mod scheduler_tests;
