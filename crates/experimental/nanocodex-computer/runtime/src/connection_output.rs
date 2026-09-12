//! One connection's ordered output. Production writes run in an owned child so
//! a blocked inherited stdout never prevents the parent from revoking authority.
//! The generic adapter below requires a finite/cooperative `Write` implementation.
use serde_json::{Value, json};
use skyre::{Error, Result, protocol};
use std::{
    collections::VecDeque,
    io::{self, BufReader, Read, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{Arc, Condvar, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::Instant,
};

const CAPACITY: usize = 16;
const CHILD_PROTOCOL: u64 = 1;
pub(crate) const CHILD_ARGUMENT: &str = "__output-worker";
type Reply = mpsc::SyncSender<Result<()>>;
type CancelTransport = Arc<dyn Fn() + Send + Sync>;

fn disconnected() -> Error {
    Error::action("Client output disconnected")
}
fn timed_out() -> Error {
    Error::new(-32002, "Client output timed out")
}

struct Job {
    id: u64,
    value: Value,
    deadline: Instant,
    reply: Reply,
}
struct Active {
    id: u64,
    deadline: Instant,
    reply: Reply,
}
#[derive(Default)]
struct State {
    next: u64,
    queue: VecDeque<Job>,
    active: Option<Active>,
    closed: bool,
    failure: Option<Error>,
}
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    cancel_transport: Option<CancelTransport>,
}
impl Shared {
    fn new(cancel_transport: Option<CancelTransport>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            cancel_transport,
        })
    }
    fn close(&self, failure: Option<Error>) {
        let (replies, error, first) = {
            let mut state = self.state.lock().unwrap();
            let first = !state.closed;
            state.closed = true;
            if state.failure.is_none() {
                state.failure = failure;
            }
            let error = state.failure.clone().unwrap_or_else(disconnected);
            let mut replies: Vec<_> = state.queue.drain(..).map(|job| job.reply).collect();
            if let Some(active) = &state.active {
                replies.push(active.reply.clone());
            }
            self.wake.notify_all();
            (replies, error, first)
        };
        // Neither acknowledgements nor cancellation depend on queue capacity.
        for reply in replies {
            let _ = reply.try_send(Err(error.clone()));
        }
        if first && let Some(cancel) = &self.cancel_transport {
            cancel();
        }
    }
    fn take(&self) -> Option<Job> {
        let mut state = self.state.lock().unwrap();
        loop {
            if state.closed {
                return None;
            }
            if let Some(job) = state.queue.pop_front() {
                state.active = Some(Active {
                    id: job.id,
                    deadline: job.deadline,
                    reply: job.reply.clone(),
                });
                self.wake.notify_all();
                return Some(job);
            }
            state = self.wake.wait(state).unwrap();
        }
    }
    fn complete(&self, job: Job, result: Result<()>) {
        let result = if Instant::now() >= job.deadline {
            Err(timed_out())
        } else {
            result
        };
        if let Err(error) = &result {
            self.close(Some(error.clone()));
        }
        let mut state = self.state.lock().unwrap();
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.id == job.id)
        {
            state.active = None;
        }
        let result = if state.closed {
            Err(state.failure.clone().unwrap_or_else(disconnected))
        } else {
            result
        };
        let _ = job.reply.try_send(result);
        self.wake.notify_all();
    }
}

#[derive(Clone)]
pub(crate) struct ConnectionOutput(Arc<Shared>);
impl ConnectionOutput {
    pub(crate) fn enqueue(
        &self,
        value: &Value,
        deadline: Instant,
    ) -> Result<mpsc::Receiver<Result<()>>> {
        if Instant::now() >= deadline {
            return Err(timed_out());
        }
        let (reply, receiver) = mpsc::sync_channel(1);
        let mut state = self.0.state.lock().unwrap();
        if state.closed {
            return Err(state.failure.clone().unwrap_or_else(disconnected));
        }
        if state.queue.len() >= CAPACITY {
            return Err(Error::action("Client output queue is full"));
        }
        let id = state.next;
        state.next = id
            .checked_add(1)
            .ok_or_else(|| Error::action("Client output sequence exhausted"))?;
        state.queue.push_back(Job {
            id,
            value: value.clone(),
            deadline,
            reply,
        });
        self.0.wake.notify_all();
        Ok(receiver)
    }

    pub(crate) fn emit(&self, value: &Value, deadline: Instant) -> Result<()> {
        let receiver = loop {
            match self.enqueue(value, deadline) {
                Ok(receiver) => break receiver,
                Err(error) if error.message == "Client output queue is full" => {
                    let state = self.0.state.lock().unwrap();
                    if !state.closed && state.queue.len() >= CAPACITY {
                        let _ = self
                            .0
                            .wake
                            .wait_timeout(state, deadline.saturating_duration_since(Instant::now()))
                            .unwrap();
                    }
                }
                Err(error) => return Err(error),
            }
        };
        match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // The write may have started. Retire the entire connection; a
                // later response cannot safely follow a possibly partial frame.
                self.0.close(Some(timed_out()));
                Err(timed_out())
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(disconnected()),
        }
    }
    pub(crate) fn status(&self) -> Result<()> {
        let state = self.0.state.lock().unwrap();
        if state.closed {
            Err(state.failure.clone().unwrap_or_else(disconnected))
        } else {
            Ok(())
        }
    }
    /// Initiate cancellation without waiting for a writer or a queue slot.
    pub(crate) fn close(&self) {
        self.0.close(None);
    }
}

fn encode(value: &Value, framed: bool) -> Result<Vec<u8>> {
    if framed {
        protocol::encode(value)
    } else {
        // JSONL output has no existing MAX_FRAME restriction. Do not introduce
        // one merely because the private worker uses length-delimited jobs.
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        Ok(bytes)
    }
}

/// Retains the child and both parent threads until cancellation has completed.
/// The watchdog never needs the command-pipe lock to kill the child.
pub(crate) struct OwnedOutput {
    pub(crate) output: ConnectionOutput,
    child: Arc<Mutex<Child>>,
    writer: Option<JoinHandle<()>>,
    watchdog: Option<JoinHandle<()>>,
    reaped: bool,
}
impl OwnedOutput {
    pub(crate) fn spawn(
        executable: &Path,
        sink: Stdio,
        framed: bool,
        cancel_transport: Option<CancelTransport>,
    ) -> Result<Self> {
        let mut child = Command::new(executable)
            .arg(CHILD_ARGUMENT)
            .stdin(Stdio::piped())
            .stdout(sink)
            // This private pipe carries acknowledgements, never user diagnostics.
            .stderr(Stdio::piped())
            .spawn()?;
        let mut input = child.stdin.take().expect("piped output-worker input");
        let mut replies = BufReader::new(child.stderr.take().expect("piped output-worker replies"));
        let child = Arc::new(Mutex::new(child));
        let shared = Shared::new(cancel_transport);
        let output = ConnectionOutput(shared.clone());
        // Construct the RAII owner before spawning either parent thread, so a
        // thread-creation failure still kills and reaps the already-owned child.
        let mut owner = Self {
            output,
            child: child.clone(),
            writer: None,
            watchdog: None,
            reaped: false,
        };
        let watched = shared.clone();
        let killed = child.clone();
        owner.watchdog = Some(
            thread::Builder::new()
                .name("client-output-deadline".into())
                .spawn(move || {
                    let mut state = watched.state.lock().unwrap();
                    loop {
                        if state.closed {
                            break;
                        }
                        let deadline = state
                            .active
                            .iter()
                            .map(|active| active.deadline)
                            .chain(state.queue.iter().map(|job| job.deadline))
                            .min();
                        match deadline {
                            Some(deadline) if Instant::now() >= deadline => {
                                drop(state);
                                watched.close(Some(timed_out()));
                                state = watched.state.lock().unwrap();
                            }
                            Some(deadline) => {
                                state = watched
                                    .wake
                                    .wait_timeout(
                                        state,
                                        deadline.saturating_duration_since(Instant::now()),
                                    )
                                    .unwrap()
                                    .0;
                            }
                            None => state = watched.wake.wait(state).unwrap(),
                        }
                    }
                    drop(state);
                    // Child owns its OS process handle. Never signal a saved raw PID or
                    // close an inherited descriptor from a different parent thread.
                    let _ = killed.lock().unwrap().kill();
                })?,
        );
        owner.writer = Some(thread::Builder::new().name("client-output".into()).spawn(move || {
            let ready = protocol::read_frame(&mut replies);
            if !matches!(ready, Ok(Some(ref ready)) if *ready == json!({"outputWorker":CHILD_PROTOCOL})) {
                shared.close(Some(Error::action("Output worker did not initialize")));
                return;
            }
            while let Some(job) = shared.take() {
                let result = (|| {
                    let bytes = encode(&job.value, framed)?;
                    if Instant::now() >= job.deadline {
                        return Err(timed_out());
                    }
                    input.write_all(&job.id.to_le_bytes())?;
                    input.write_all(&(bytes.len() as u64).to_le_bytes())?;
                    input.write_all(&bytes)?;
                    input.flush()?;
                    drop(bytes);
                    let reply = protocol::read_frame(&mut replies)?.ok_or_else(disconnected)?;
                    protocol::validate_response(&reply)?;
                    if reply["id"].as_u64() != Some(job.id) {
                        return Err(Error::action("Output worker acknowledgement mismatch"));
                    }
                    if let Some(error) = reply.get("error") {
                        return Err(Error::new(
                            error["code"].as_i64().and_then(|code| code.try_into().ok()).unwrap_or(-32000),
                            error["message"].as_str().unwrap_or("Client output failed"),
                        ));
                    }
                    Ok(())
                })();
                shared.complete(job, result);
            }
        })?);
        Ok(owner)
    }

    /// Call after the server has revoked its connection authority. This waits
    /// only on owned output machinery; generic input cancellation is separate.
    pub(crate) fn join(&mut self) -> Result<()> {
        if self.reaped && self.watchdog.is_none() && self.writer.is_none() {
            return Ok(());
        }
        self.output.close();
        let watchdog = self.watchdog.take().map(|thread| thread.join());
        let status = {
            let mut child = self.child.lock().unwrap();
            // Also covers watchdog startup failure or panic. This handle has
            // not been reaped, so it cannot name a subsequently reused process.
            let _ = child.kill();
            child.wait()
        };
        self.reaped = status.is_ok();
        let writer = self.writer.take().map(|thread| thread.join());
        if watchdog.is_some_and(|result| result.is_err())
            || writer.is_some_and(|result| result.is_err())
        {
            return Err(Error::action("Output worker thread panicked"));
        }
        status?;
        Ok(())
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn child_id(&self) -> u32 {
        self.child.lock().unwrap().id()
    }
}
impl Drop for OwnedOutput {
    fn drop(&mut self) {
        let _ = self.join();
    }
}

/// Generic embeddings may supply finite writers such as Vec, Cursor or a
/// deterministic failing writer. Arbitrary Rust Write/flush cannot be preempted.
#[cfg(test)]
pub(crate) fn with_finite_writer<T>(
    writer: &mut (impl Write + Send),
    framed: bool,
    run: impl FnOnce(&ConnectionOutput) -> T,
) -> T {
    thread::scope(|scope| {
        let shared = Shared::new(None);
        let output = ConnectionOutput(shared.clone());
        struct Close(ConnectionOutput);
        impl Drop for Close {
            fn drop(&mut self) {
                self.0.close();
            }
        }
        let _close = Close(output.clone());
        scope.spawn(move || {
            while let Some(job) = shared.take() {
                let result = (|| {
                    if Instant::now() >= job.deadline {
                        return Err(timed_out());
                    }
                    // Keep the original generic writer's serialization and
                    // error conversion, including serde's JSONL write errors.
                    if framed {
                        protocol::write_frame(writer, &job.value)?;
                    } else {
                        serde_json::to_writer(&mut *writer, &job.value)?;
                        writeln!(writer)?;
                        writer.flush()?;
                    }
                    Ok(())
                })();
                shared.complete(job, result);
            }
        });
        run(&output)
    })
}

/// Private executable entry point. Byte jobs preserve the existing external
/// framing, including JSONL output larger than the native 8 MiB frame limit.
pub(crate) fn run_child() -> ! {
    let (send, jobs) = mpsc::sync_channel(1);
    // Keep reading control input while stdout is blocked. EOF means the parent
    // has retired this worker (or died); terminate the whole owned child rather
    // than leaving an orphan blocked on a client that will never read again.
    let _reader = match thread::Builder::new()
        .name("output-control".into())
        .spawn(move || {
            let result = read_jobs(&mut io::stdin().lock(), send);
            std::process::exit(if result.is_ok() { 0 } else { 1 });
        }) {
        Ok(reader) => reader,
        Err(_) => std::process::exit(1),
    };
    let result = (|| -> Result<()> {
        let mut output = io::stdout().lock();
        let mut replies = io::stderr().lock();
        protocol::write_frame(&mut replies, &json!({"outputWorker":CHILD_PROTOCOL}))?;
        while let Ok((id, bytes)) = jobs.recv() {
            let result = output
                .write_all(&bytes)
                .and_then(|()| output.flush())
                .map(|()| Value::Null)
                .map_err(Error::from);
            let failed = result.is_err();
            protocol::write_frame(&mut replies, &protocol::response(json!(id), result))?;
            if failed {
                return Err(Error::action("Client output failed"));
            }
        }
        Err(disconnected())
    })();
    // The private control reader belongs to this process. Its handle is retained
    // until process exit; no parent writer/reader thread is abandoned here.
    std::process::exit(if result.is_ok() { 0 } else { 1 });
}

fn read_jobs(input: &mut impl Read, send: mpsc::SyncSender<(u64, Vec<u8>)>) -> Result<()> {
    loop {
        let mut header = [0u8; 16];
        if input.read(&mut header[..1])? == 0 {
            return Ok(());
        }
        input.read_exact(&mut header[1..])?;
        let id = u64::from_le_bytes(header[..8].try_into().unwrap());
        let length = u64::from_le_bytes(header[8..].try_into().unwrap());
        let mut bytes = Vec::new();
        // Grow only from received bytes. The private protocol has no smaller
        // output ceiling than JSONL, and it never trusts a length for allocation.
        (&mut *input).take(length).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != length {
            return Err(Error::invalid("Truncated output worker job"));
        }
        // The trusted parent sends one job and waits for its acknowledgement.
        // Unexpected pipelining must not block this lifetime/EOF reader.
        send.try_send((id, bytes))
            .map_err(|_| Error::invalid("Output worker job queue is full"))?;
    }
}
