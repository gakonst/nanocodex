//! Rust-only runtime child. The parent owns services and authorizes every RPC.
//! This contains process-fatal engine errors; it is not a filesystem sandbox.
use crate::{
    Error, Result,
    runtime::{Host, HostOptions, ProviderControl, RuntimeBackend},
    worker::{Command, Event},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command as ProcessCommand, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
const FRAME_LIMIT: usize = 16 * 1024 * 1024;
const CANCEL_GRACE: Duration = Duration::from_millis(250);
#[derive(Debug, Serialize, Deserialize)]
struct Failure {
    code: i32,
    message: String,
}
impl From<Error> for Failure {
    fn from(e: Error) -> Self {
        Self {
            code: e.code,
            message: e.message,
        }
    }
}
impl From<Failure> for Error {
    fn from(e: Failure) -> Self {
        Self::new(e.code, e.message)
    }
}
type WireResult = std::result::Result<Value, Failure>;
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum Frame {
    Init {
        options: HostOptions,
    },
    Ready {},
    Started {
        ticket: u64,
    },
    Heartbeat {},
    Suspension {
        start: bool,
    },
    Eval {
        ticket: u64,
        code: String,
        timeout_ms: u64,
        completion: bool,
    },
    Metadata {
        value: Option<Value>,
    },
    Tick {
        budget_ms: u64,
    },
    Ticked {
        kernel_reset: bool,
        pending: bool,
    },
    Updated {
        result: WireResult,
    },
    Cancel {},
    Call {
        id: u64,
        method: String,
        args: Value,
        drain_proof: Option<crate::runtime::DrainProof>,
        activation_model: crate::browser_activation::Model,
    },
    CallSuspension {
        id: u64,
        control_id: u64,
        start: bool,
    },
    CallSuspended {
        control_id: u64,
        result: WireResult,
    },
    Reply {
        id: u64,
        result: WireResult,
        continuation: Option<String>,
    },
    Done {
        ticket: u64,
        result: WireResult,
        kernel_reset: bool,
        pending: bool,
    },
}
fn read_frame(reader: &mut impl Read) -> Result<Frame> {
    let mut length = [0u8; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > FRAME_LIMIT {
        return Err(Error::invalid("Runtime IPC frame exceeds limit"));
    }
    let mut data = vec![0; length];
    reader.read_exact(&mut data)?;
    Ok(serde_json::from_slice(&data)?)
}
struct BoundedBuffer(Vec<u8>);
impl Write for BoundedBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > FRAME_LIMIT.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other("Runtime IPC frame exceeds limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn write_frame(writer: &mut impl Write, value: &Frame) -> Result<()> {
    let mut data = BoundedBuffer(Vec::new());
    serde_json::to_writer(&mut data, value)?;
    let data = data.0;
    writer.write_all(&(data.len() as u32).to_be_bytes())?;
    writer.write_all(&data)?;
    writer.flush()?;
    Ok(())
}
struct Process {
    child: Arc<Mutex<Child>>,
    watchdog: crate::worker_watchdog::Watchdog,
    stderr_reader: Option<JoinHandle<()>>,
    input: Arc<Mutex<ChildStdin>>,
    suspension_acks: Arc<Mutex<BTreeMap<u64, mpsc::SyncSender<Result<Value>>>>>,
    next_control: Arc<AtomicU64>,
    output: Receiver<Result<Frame>>,
    reader: Option<JoinHandle<()>>,
    completed: bool,
    background: bool,
}
impl Process {
    fn launch(executable: &Path, options: &HostOptions, cancel: &Arc<AtomicBool>) -> Result<Self> {
        let mut child = ProcessCommand::new(executable)
            .arg("__runtime-worker")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| Error::action("Missing runtime child input"))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::action("Missing runtime child output"))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::action("Missing runtime child stderr"))?;
        // Always drain child diagnostics without depending on the parent's stderr sink.
        let stderr_reader = thread::spawn(move || {
            let mut bytes = [0u8; 4096];
            while matches!(stderr.read(&mut bytes),Ok(n) if n>0) {}
        });
        let child = Arc::new(Mutex::new(child));
        let watchdog = crate::worker_watchdog::Watchdog::new(child.clone(), cancel.clone());
        watchdog.arm(None, Duration::from_secs(10))?;
        let observer = watchdog.observer();
        let (send, output) = mpsc::sync_channel(1);
        let suspension_acks = Arc::new(Mutex::new(
            BTreeMap::<u64, mpsc::SyncSender<Result<Value>>>::new(),
        ));
        let pending_acks = suspension_acks.clone();
        let reader = thread::spawn(move || {
            loop {
                let frame = match read_frame(&mut stdout) {
                    Ok(Frame::CallSuspended { control_id, result }) => {
                        if let Some(reply) = pending_acks.lock().unwrap().remove(&control_id) {
                            let _ = reply.send(result.map_err(Error::from));
                            continue;
                        }
                        Err(Error::invalid(
                            "Unexpected provider suspension acknowledgement",
                        ))
                    }
                    Ok(Frame::Heartbeat {}) => {
                        observer.heartbeat();
                        continue;
                    }
                    Ok(Frame::Started { ticket }) => match observer.started(ticket) {
                        Ok(()) => continue,
                        Err(error) => Err(error),
                    },
                    Ok(Frame::Suspension { start }) => match observer.suspend(start) {
                        Ok(()) => continue,
                        Err(error) => Err(error),
                    },
                    other => other,
                };
                let failed = frame.is_err();
                if send.send(frame).is_err() || failed {
                    break;
                }
            }
        });
        let process = Self {
            child,
            watchdog,
            stderr_reader: Some(stderr_reader),
            input: Arc::new(Mutex::new(input)),
            suspension_acks,
            next_control: Arc::new(AtomicU64::new(0)),
            output,
            reader: Some(reader),
            completed: false,
            background: false,
        };
        write_frame(
            &mut *process.input.lock().unwrap(),
            &Frame::Init {
                options: options.clone(),
            },
        )?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(Error::new(
                    -32800,
                    "Evaluation cancelled during runtime startup",
                ));
            }
            match process.output.recv_timeout(Duration::from_millis(5)) {
                Ok(Ok(Frame::Ready {})) => {
                    process.watchdog.idle();
                    return Ok(process);
                }
                Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
                _ => return Err(Error::action("Runtime child did not initialize")),
            }
        }
    }
    fn evaluate(
        &mut self,
        ticket: u64,
        code: String,
        evaluation: (Duration, bool),
        cancel: &Arc<AtomicBool>,
        events: &Sender<Event>,
        lost: &AtomicBool,
    ) -> Result<Value> {
        let (timeout, completion) = evaluation;
        self.completed = false;
        self.watchdog.arm(Some(ticket), timeout)?;
        let timeout_ms = u64::try_from(timeout.as_millis())
            .map_err(|_| Error::invalid("Runtime timeout exceeds u64 milliseconds"))?;
        write_frame(
            &mut *self.input.lock().unwrap(),
            &Frame::Eval {
                ticket,
                code,
                timeout_ms,
                completion,
            },
        )?;
        let mut cancelling = None;
        loop {
            if cancel.load(Ordering::Acquire) {
                if cancelling.is_none() {
                    cancelling = Some(Instant::now());
                    write_frame(&mut *self.input.lock().unwrap(), &Frame::Cancel {})?;
                }
                if cancelling.is_some_and(|start: Instant| start.elapsed() >= CANCEL_GRACE) {
                    return Ok(cancelled_response());
                }
            }
            match self.output.recv_timeout(Duration::from_millis(5)) {
                Ok(Ok(Frame::Done {
                    ticket: actual,
                    result,
                    kernel_reset,
                    pending,
                })) if ticket == actual => {
                    self.completed = true;
                    self.background = pending;
                    let reason = self.watchdog.reason();
                    self.watchdog.idle();
                    if reason != 0 {
                        lost.store(true, Ordering::Release);
                    }
                    if kernel_reset {
                        lost.store(true, Ordering::Release);
                    }
                    return if cancelling.is_some() || reason == 1 || reason == 2 {
                        Ok(cancelled_response())
                    } else {
                        result.map_err(Error::from)
                    };
                }
                Ok(Ok(Frame::Call {
                    id,
                    method,
                    args,
                    drain_proof,
                    activation_model,
                })) => {
                    let input = self.input.clone();
                    let acks = self.suspension_acks.clone();
                    let counter = self.next_control.clone();
                    let cancelled = cancel.clone();
                    let execution_observer = self.watchdog.observer();
                    let control = ProviderControl::new_with_activation_model(
                        move |start| {
                            if start && cancelled.load(Ordering::Acquire) {
                                return Err(Error::new(-32800, "Evaluation cancelled"));
                            }
                            let control_id = counter
                                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                                    n.checked_add(1)
                                })
                                .map_err(|_| Error::action("Provider control counter exhausted"))?
                                + 1;
                            let (reply, response) = mpsc::sync_channel(1);
                            {
                                let mut acks = acks.lock().unwrap();
                                if acks.len() >= 16 {
                                    return Err(Error::action(
                                        "Provider control acknowledgement limit exceeded",
                                    ));
                                }
                                acks.insert(control_id, reply);
                            }
                            let result = write_frame(
                                &mut *input.lock().unwrap(),
                                &Frame::CallSuspension {
                                    id,
                                    control_id,
                                    start,
                                },
                            )
                            .and_then(|_| {
                                response.recv_timeout(Duration::from_secs(2)).map_err(|_| {
                                    Error::action("Provider suspension was not acknowledged")
                                })?
                            });
                            acks.lock().unwrap().remove(&control_id);
                            if result.is_err() {
                                cancelled.store(true, Ordering::Release);
                            }
                            result.map(|_| ())
                        },
                        Some(Arc::new(move || {
                            // Capture this evaluation's native ticket, not a wire
                            // value or an estimate based on its original timeout.
                            execution_observer.validate_execution(ticket)
                        })),
                        activation_model,
                    );
                    control.set_drain_proof(drain_proof)?;
                    let lifetime = control.lifetime();
                    let result = if cancel.load(Ordering::Acquire) {
                        Err(Error::new(-32800, "Evaluation cancelled"))
                    } else {
                        let (reply, receive) = mpsc::sync_channel(1);
                        events
                            .send(Event::Call {
                                method,
                                args,
                                reply,
                                control: control.clone(),
                            })
                            .map_err(|_| Error::action("Service owner disconnected"))?;
                        loop {
                            match receive.recv_timeout(Duration::from_millis(5)) {
                                Ok(result) => break result,
                                Err(mpsc::RecvTimeoutError::Timeout)
                                    if !cancel.load(Ordering::Acquire)
                                        && self.watchdog.reason() == 0 => {}
                                Err(mpsc::RecvTimeoutError::Timeout) => {
                                    break Err(Error::new(-32800, "Evaluation cancelled"));
                                }
                                Err(_) => break Err(Error::action("Service reply disconnected")),
                            }
                        }
                    };
                    lifetime.finish()?;
                    write_frame(
                        &mut *self.input.lock().unwrap(),
                        &Frame::Reply {
                            id,
                            result: result.map_err(Failure::from),
                            continuation: control.continuation(),
                        },
                    )?;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Ok(Err(error)) => {
                    if self.watchdog.reason() == 2 {
                        return Ok(cancelled_response());
                    }
                    return Err(Error::new(
                        -32008,
                        format!("Runtime child terminated; kernel reset ({})", error.message),
                    ));
                }
                _ => {
                    return Err(Error::new(
                        -32008,
                        "Runtime child protocol failed; kernel reset",
                    ));
                }
            }
        }
    }
    fn tick(&mut self, budget: Duration) -> Result<bool> {
        self.completed = false;
        self.watchdog.arm(Some(0), budget)?;
        write_frame(
            &mut *self.input.lock().unwrap(),
            &Frame::Tick {
                budget_ms: budget.as_millis() as u64,
            },
        )?;
        loop {
            match self.output.recv_timeout(Duration::from_millis(5)) {
                Ok(Ok(Frame::Ticked {
                    kernel_reset,
                    pending,
                })) => {
                    self.completed = true;
                    self.background = pending;
                    self.watchdog.idle();
                    if kernel_reset {
                        return Err(Error::action("Background task failed; kernel reset"));
                    }
                    return Ok(pending);
                }
                Err(mpsc::RecvTimeoutError::Timeout) if self.watchdog.reason() == 0 => {}
                _ => return Err(Error::action("Background child stopped; kernel reset")),
            }
        }
    }
    fn metadata(&mut self, value: Option<Value>) -> Result<()> {
        self.completed = false;
        self.watchdog.arm(None, Duration::from_secs(3))?;
        write_frame(&mut *self.input.lock().unwrap(), &Frame::Metadata { value })?;
        match self.output.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(Frame::Updated { result })) => {
                self.completed = true;
                self.watchdog.idle();
                result.map(|_| ()).map_err(Error::from)
            }
            _ => Err(Error::action("Runtime child metadata update failed")),
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        // Disconnect first so an in-flight bounded send cannot block the reader join.
        let (_, empty) = mpsc::channel();
        drop(std::mem::replace(&mut self.output, empty));
        self.watchdog.stop();
        {
            let mut child = self.child.lock().unwrap();
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
        }
    }
}
fn cancelled_response() -> Value {
    serde_json::json!({"outputs":[],"responseMeta":{},"executionDurationMs":0,"exceptionMessage":"Evaluation cancelled or timed out","error":{"code":-32004,"message":"Evaluation cancelled or timed out"}})
}
pub(super) fn supervise(
    executable: PathBuf,
    mut options: HostOptions,
    commands: Receiver<Command>,
    events: Sender<Event>,
    cancel: Arc<AtomicBool>,
    lost: Arc<AtomicBool>,
    child_pid: Arc<AtomicU32>,
) {
    let mut process: Option<Process> = None;
    loop {
        let incoming = if process.as_ref().is_some_and(|process| process.background) {
            commands.recv_timeout(Duration::from_millis(10))
        } else {
            commands
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        };
        let command = match incoming {
            Ok(command) => command,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(current) = process.as_mut()
                    && current.background
                    && current.tick(Duration::from_millis(100)).is_err()
                {
                    process = None;
                    child_pid.store(0, Ordering::Release);
                    lost.store(true, Ordering::Release);
                }
                continue;
            }
        };
        match command {
            Command::Eval {
                ticket,
                code,
                timeout,
                completion,
            } => {
                let evaluation_lost = AtomicBool::new(false);
                let result = (|| {
                    if process.is_none() {
                        process = Some(Process::launch(&executable, &options, &cancel)?);
                        child_pid.store(
                            process.as_ref().unwrap().child.lock().unwrap().id(),
                            Ordering::Release,
                        );
                    }
                    process.as_mut().unwrap().evaluate(
                        ticket,
                        code,
                        (timeout, completion),
                        &cancel,
                        &events,
                        &evaluation_lost,
                    )
                })();
                let stopped = process.as_ref().map_or(0, |p| p.watchdog.reason());
                let result = match stopped {
                    1 | 2 => Ok(cancelled_response()),
                    3 => Err(Error::new(
                        -32008,
                        "Runtime child heartbeat stopped; kernel reset",
                    )),
                    _ => result,
                };
                if process.as_ref().is_none_or(|p| !p.completed)
                    || cancel.load(Ordering::Acquire)
                    || evaluation_lost.load(Ordering::Acquire)
                    || stopped != 0
                {
                    process = None;
                    child_pid.store(0, Ordering::Release);
                    lost.store(true, Ordering::Release);
                }
                let result = if cancel.load(Ordering::Acquire) {
                    Ok(cancelled_response())
                } else {
                    result
                };
                if events.send(Event::Done { ticket, result }).is_err() {
                    break;
                }
            }
            Command::Metadata { value, reply } => {
                let result = process
                    .as_mut()
                    .map_or(Ok(()), |p| p.metadata(value.clone()));
                if result.is_ok() {
                    options.request_meta = value;
                } else if process.as_ref().is_none_or(|p| !p.completed) {
                    process = None;
                    child_pid.store(0, Ordering::Release);
                    lost.store(true, Ordering::Release);
                }
                let _ = reply.send(result);
            }
            Command::Reset => {
                process = None;
                child_pid.store(0, Ordering::Release);
            }
            Command::Stop => break,
        }
    }
}
struct Heartbeat {
    state: Arc<(Mutex<(bool, bool)>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}
impl Heartbeat {
    fn new(writer: Arc<Mutex<std::io::Stdout>>) -> Self {
        let state = Arc::new((Mutex::new((false, false)), Condvar::new()));
        let shared = state.clone();
        let thread = thread::spawn(move || {
            let (lock, wake) = &*shared;
            let mut state = lock.lock().unwrap();
            loop {
                if state.1 {
                    break;
                }
                if !state.0 {
                    state = wake.wait(state).unwrap();
                    continue;
                }
                let (next, elapsed) = wake
                    .wait_timeout(state, Duration::from_millis(100))
                    .unwrap();
                state = next;
                if state.0 && elapsed.timed_out() {
                    drop(state);
                    if write_frame(&mut *writer.lock().unwrap(), &Frame::Heartbeat {}).is_err() {
                        break;
                    }
                    state = lock.lock().unwrap();
                }
            }
        });
        Self {
            state,
            thread: Some(thread),
        }
    }
    fn active(&self, value: bool) {
        self.state.0.lock().unwrap().0 = value;
        self.state.1.notify_all();
    }
}
impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.state.0.lock().unwrap().1 = true;
        self.state.1.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
/// Dedicated child entrypoint. It has no Engine, desktop, browser or approval state.
pub fn run_child() -> Result<()> {
    let mut stdin = std::io::stdin();
    let Frame::Init { options } = read_frame(&mut stdin)? else {
        return Err(Error::invalid("Expected runtime child initialization"));
    };
    if options.runtime != RuntimeBackend::V8 {
        return Err(Error::invalid("Process runtime requires V8"));
    }
    options.runtime.require_available()?;
    let writer = Arc::new(Mutex::new(std::io::stdout()));
    let cancel = Arc::new(AtomicBool::new(false));
    let (commands, receive) = mpsc::sync_channel(1);
    let (replies, responses) = mpsc::sync_channel(1);
    let interrupted = cancel.clone();
    thread::spawn(move || {
        while let Ok(frame) = read_frame(&mut stdin) {
            let sent = match frame {
                Frame::Cancel {} => {
                    interrupted.store(true, Ordering::Release);
                    true
                }
                Frame::Reply { .. } | Frame::CallSuspension { .. } => {
                    replies.try_send(frame).is_ok()
                }
                Frame::Eval { .. } => {
                    interrupted.store(false, Ordering::Release);
                    commands.try_send(frame).is_ok()
                }
                Frame::Metadata { .. } | Frame::Tick { .. } => commands.try_send(frame).is_ok(),
                _ => false,
            };
            if !sent {
                break;
            }
        }
        interrupted.store(true, Ordering::Release);
    });
    let heartbeat = Heartbeat::new(writer.clone());
    let mut options = options;
    let responses = std::rc::Rc::new(responses);
    let mut host = None;
    write_frame(&mut *writer.lock().unwrap(), &Frame::Ready {})?;
    while let Ok(frame) = receive.recv() {
        match frame {
            Frame::Eval {
                ticket,
                code,
                timeout_ms,
                completion,
            } => {
                // The parent sends a new Eval only after the preceding Done.
                heartbeat.active(true);
                let result = (|| {
                    if host.is_none() {
                        let rpc_writer = writer.clone();
                        let responses = responses.clone();
                        let cancelled = cancel.clone();
                        let mut next = 0u64;
                        host = Some(Host::with_controlled_dispatch(
                            move |method, args, control| {
                                if cancelled.load(Ordering::Acquire) {
                                    return Err(Error::new(-32800, "Evaluation cancelled"));
                                }
                                next = next
                                    .checked_add(1)
                                    .ok_or_else(|| Error::action("Runtime RPC counter overflow"))?;
                                write_frame(
                                    &mut *rpc_writer.lock().unwrap(),
                                    &Frame::Call {
                                        id: next,
                                        method: method.into(),
                                        args: args.clone(),
                                        drain_proof: control.drain_proof(),
                                        activation_model: control.activation_model()?.clone(),
                                    },
                                )?;
                                loop {
                                    match responses.recv_timeout(Duration::from_millis(5)) {
                                        Ok(Frame::CallSuspension {
                                            id,
                                            control_id,
                                            start,
                                        }) if id == next => {
                                            let result = control
                                                .change(start)
                                                .map(|_| Value::Null)
                                                .map_err(Failure::from);
                                            write_frame(
                                                &mut *rpc_writer.lock().unwrap(),
                                                &Frame::CallSuspended { control_id, result },
                                            )?;
                                        }
                                        Ok(Frame::Reply {
                                            id,
                                            result,
                                            continuation,
                                        }) if id == next => {
                                            if let Some(id) = continuation {
                                                control.bind_continuation(id)?;
                                            }
                                            return result.map_err(Error::from);
                                        }
                                        Err(mpsc::RecvTimeoutError::Timeout)
                                            if !cancelled.load(Ordering::Acquire) => {}
                                        Err(mpsc::RecvTimeoutError::Timeout) => {
                                            return Err(Error::new(-32800, "Evaluation cancelled"));
                                        }
                                        _ => {
                                            return Err(Error::action(
                                                "Runtime service reply disconnected",
                                            ));
                                        }
                                    }
                                }
                            },
                            cancel.clone(),
                            options.clone(),
                        )?);
                        let writer = writer.clone();
                        host.as_mut().unwrap().set_timeout_observer(move |start| {
                            write_frame(&mut *writer.lock().unwrap(), &Frame::Suspension { start })
                        })?;
                    }
                    write_frame(&mut *writer.lock().unwrap(), &Frame::Started { ticket })?;
                    host.as_mut().unwrap().evaluate_mode(
                        &code,
                        Duration::from_millis(timeout_ms),
                        completion,
                    )
                })();
                heartbeat.active(false);
                let kernel_reset = host.as_ref().is_some_and(Host::interrupted);
                if kernel_reset {
                    host = None;
                }
                write_frame(
                    &mut *writer.lock().unwrap(),
                    &Frame::Done {
                        ticket,
                        result: result.map_err(Failure::from),
                        kernel_reset,
                        pending: host.as_ref().is_some_and(Host::has_background),
                    },
                )?;
            }
            Frame::Tick { budget_ms } => {
                heartbeat.active(true);
                write_frame(&mut *writer.lock().unwrap(), &Frame::Started { ticket: 0 })?;
                let failed = host.as_mut().is_some_and(|host| {
                    host.tick(Duration::from_millis(budget_ms.min(100)))
                        .is_err()
                        || host.interrupted()
                });
                heartbeat.active(false);
                if failed {
                    host = None;
                }
                write_frame(
                    &mut *writer.lock().unwrap(),
                    &Frame::Ticked {
                        kernel_reset: failed,
                        pending: host.as_ref().is_some_and(Host::has_background),
                    },
                )?;
            }
            Frame::Metadata { value } => {
                let result = host
                    .as_mut()
                    .map_or(Ok(()), |h| h.set_request_meta(value.clone()));
                if result.is_ok() {
                    options.request_meta = value;
                }
                write_frame(
                    &mut *writer.lock().unwrap(),
                    &Frame::Updated {
                        result: result.map(|_| Value::Null).map_err(Failure::from),
                    },
                )?;
            }
            _ => return Err(Error::invalid("Unexpected runtime child command")),
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framed_call_requires_a_valid_native_activation_projection() {
        use crate::browser_activation::Model;
        let base = serde_json::json!({"kind":"Call","id":1,"method":"browser.info","args":{},"drain_proof":null});
        assert!(serde_json::from_value::<Frame>(base.clone()).is_err());
        for model in [
            serde_json::json!(null),
            serde_json::json!({"kind":"unknown"}),
            serde_json::json!({"kind":"compatible","extra":true}),
            serde_json::json!({"kind":"incompatible","model":"GPT-LUNA"}),
            serde_json::json!({"kind":"incompatible","model":"x".repeat(65536)+"-luna"}),
        ] {
            let mut invalid = base.clone();
            invalid["activation_model"] = model;
            let json = invalid.to_string();
            let mut bytes = (json.len() as u32).to_be_bytes().to_vec();
            bytes.extend(json.as_bytes());
            assert!(read_frame(&mut bytes.as_slice()).is_err());
        }
        for model in [
            Model::Compatible,
            Model::Incompatible {
                model: "gpt-luna".into(),
            },
            Model::Unavailable {
                reason: crate::browser_activation::Unavailable::ModelTooLarge,
            },
            Model::Unavailable {
                reason: crate::browser_activation::Unavailable::InvalidMetadata,
            },
        ] {
            let mut bytes = vec![];
            write_frame(
                &mut bytes,
                &Frame::Call {
                    id: 7,
                    method: "browser.info".into(),
                    args: serde_json::json!({}),
                    drain_proof: None,
                    activation_model: model.clone(),
                },
            )
            .unwrap();
            match read_frame(&mut bytes.as_slice()).unwrap() {
                Frame::Call {
                    id: 7,
                    activation_model,
                    ..
                } => assert_eq!(activation_model, model),
                _ => panic!("Expected unchanged native call projection"),
            }
        }
    }
    #[test]
    fn framed_ipc_rejects_oversize_before_body_allocation() {
        assert!(
            read_frame(&mut (FRAME_LIMIT as u32 + 1).to_be_bytes().as_slice())
                .unwrap_err()
                .message
                .contains("limit")
        );
    }
    #[test]
    fn framed_ipc_bounds_serialization_without_writing_partial_frame() {
        let mut output = vec![];
        assert!(
            write_frame(
                &mut output,
                &Frame::Eval {
                    ticket: 1,
                    code: "x".repeat(FRAME_LIMIT),
                    timeout_ms: 1,
                    completion: true,
                }
            )
            .is_err()
        );
        assert!(output.is_empty());
    }
    #[test]
    fn framed_ipc_rejects_truncation_unknown_fields_and_roundtrips() {
        let mut bytes = vec![];
        write_frame(
            &mut bytes,
            &Frame::Done {
                ticket: 7,
                result: Err(Failure {
                    code: -1,
                    message: "owned β".into(),
                }),
                kernel_reset: false,
                pending: false,
            },
        )
        .unwrap();
        assert!(matches!(
            read_frame(&mut bytes.as_slice()).unwrap(),
            Frame::Done { ticket: 7, .. }
        ));
        assert!(read_frame(&mut &bytes[..bytes.len() - 1]).is_err());
        for json in [
            r#"{"kind":"Ready","unexpected":true}"#,
            r#"{"kind":"Suspension","start":1}"#,
            r#"{"kind":"Suspension","start":true,"ticket":7}"#,
            r#"{"kind":"Heartbeat","unexpected":true}"#,
        ] {
            let mut bytes = (json.len() as u32).to_be_bytes().to_vec();
            bytes.extend(json.as_bytes());
            assert!(read_frame(&mut bytes.as_slice()).is_err(), "{json}");
        }
    }
}
