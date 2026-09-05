use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc as std_mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use rquickjs::{
    CatchResultExt, Context, Ctx, Exception, Function, Persistent, Promise, Runtime,
    function::Func, promise::PromiseState,
};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;

use super::RuntimeEvent;

const BOOTSTRAP: &str = include_str!("bootstrap.js");

type SavedFunction = Persistent<Function<'static>>;

pub(super) struct EmbeddedHost {
    command_tx: std_mpsc::Sender<HostCommand>,
    events: mpsc::UnboundedReceiver<RuntimeEvent>,
    interrupted: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

enum HostCommand {
    Start(StartExecution),
    ToolResult {
        execution_id: u64,
        id: u64,
        value: Value,
        success: bool,
    },
    Shutdown,
}

struct StartExecution {
    execution_id: u64,
    source: String,
    tools: Vec<Value>,
    stored: HashMap<String, Value>,
}

struct ExecutionState {
    execution_id: u64,
    event_tx: mpsc::UnboundedSender<RuntimeEvent>,
    pending_tools: HashMap<u64, (SavedFunction, SavedFunction)>,
    pending_timeouts: HashMap<u32, PendingTimeout>,
    next_tool_id: u64,
    next_timeout_id: u32,
}

struct PendingTimeout {
    callback: SavedFunction,
    deadline: Option<Instant>,
}

enum CommandPoll {
    Command(HostCommand),
    TimerReady,
    Disconnected,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ExecutionTerminal {
    Done {
        #[serde(default)]
        stored: HashMap<String, Value>,
    },
    Error {
        message: String,
        #[serde(default)]
        stored: HashMap<String, Value>,
    },
}

impl EmbeddedHost {
    pub(super) fn spawn() -> Result<Self, String> {
        let (command_tx, command_rx) = std_mpsc::channel();
        let (event_tx, events) = mpsc::unbounded_channel();
        let (ready_tx, ready_rx) = std_mpsc::sync_channel(1);
        let interrupted = Arc::new(AtomicBool::new(false));
        let worker_interrupted = Arc::clone(&interrupted);
        let worker = thread::Builder::new()
            .name("nanocodex-code-mode-quickjs".to_owned())
            .spawn(move || {
                run_worker(&command_rx, &event_tx, &ready_tx, &worker_interrupted);
            })
            .map_err(|error| format!("failed to start embedded QuickJS code-mode host: {error}"))?;
        ready_rx
            .recv()
            .map_err(|_| "embedded QuickJS code-mode host ended during startup".to_owned())??;
        Ok(Self {
            command_tx,
            events,
            interrupted,
            worker: Some(worker),
        })
    }

    pub(super) fn start_cell(
        &self,
        execution_id: u64,
        source: &str,
        stored: HashMap<String, Value>,
        tools: Vec<Value>,
    ) -> Result<(), String> {
        self.command_tx
            .send(HostCommand::Start(StartExecution {
                execution_id,
                source: source.to_owned(),
                tools,
                stored,
            }))
            .map_err(|_| "embedded QuickJS code-mode host is unavailable".to_owned())
    }

    pub(super) async fn read_event(&mut self) -> Result<RuntimeEvent, String> {
        self.events
            .recv()
            .await
            .ok_or_else(|| "embedded QuickJS code-mode host ended before a result".to_owned())
    }

    pub(super) fn send_tool_result(
        &self,
        execution_id: u64,
        id: u64,
        value: Value,
        success: bool,
    ) -> Result<(), String> {
        self.command_tx
            .send(HostCommand::ToolResult {
                execution_id,
                id,
                value,
                success,
            })
            .map_err(|_| {
                "embedded QuickJS code-mode host closed before accepting a tool result".into()
            })
    }

    pub(super) async fn terminate(&mut self) {
        self.interrupted.store(true, Ordering::Release);
        let _ = self.command_tx.send(HostCommand::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = tokio::task::spawn_blocking(move || worker.join()).await;
        }
    }
}

impl Drop for EmbeddedHost {
    fn drop(&mut self) {
        self.interrupted.store(true, Ordering::Release);
        let _ = self.command_tx.send(HostCommand::Shutdown);
    }
}

fn run_worker(
    command_rx: &std_mpsc::Receiver<HostCommand>,
    event_tx: &mpsc::UnboundedSender<RuntimeEvent>,
    ready_tx: &std_mpsc::SyncSender<Result<(), String>>,
    interrupted: &Arc<AtomicBool>,
) {
    let runtime = match Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready_tx.send(Err(format!(
                "failed to initialize embedded QuickJS runtime: {error}"
            )));
            return;
        }
    };
    let interrupt_signal = Arc::clone(interrupted);
    runtime.set_interrupt_handler(Some(Box::new(move || {
        interrupt_signal.load(Ordering::Acquire)
    })));
    let prewarmed_context = match Context::full(&runtime) {
        Ok(context) => context,
        Err(error) => {
            let _ = ready_tx.send(Err(format!(
                "failed to create embedded QuickJS context: {error}"
            )));
            return;
        }
    };
    drop(prewarmed_context);
    runtime.run_gc();
    if ready_tx.send(Ok(())).is_err() {
        return;
    }

    loop {
        match command_rx.recv() {
            Ok(HostCommand::Start(start)) => {
                interrupted.store(false, Ordering::Release);
                let context = match Context::full(&runtime) {
                    Ok(context) => context,
                    Err(error) => {
                        tracing::error!(
                            target: "nanocodex_tools",
                            %error,
                            "failed to create a fresh embedded QuickJS context"
                        );
                        return;
                    }
                };
                let result = run_execution(&context, start, command_rx, event_tx.clone());
                drop(context);
                runtime.run_gc();
                if result.is_err() {
                    return;
                }
            }
            Ok(HostCommand::ToolResult { .. }) => {}
            Ok(HostCommand::Shutdown) | Err(_) => return,
        }
    }
}

fn run_execution(
    context: &Context,
    start: StartExecution,
    command_rx: &std_mpsc::Receiver<HostCommand>,
    event_tx: mpsc::UnboundedSender<RuntimeEvent>,
) -> Result<(), String> {
    context.with(|ctx| run_execution_in_context(&ctx, start, command_rx, event_tx))
}

fn run_execution_in_context<'js>(
    ctx: &Ctx<'js>,
    start: StartExecution,
    command_rx: &std_mpsc::Receiver<HostCommand>,
    event_tx: mpsc::UnboundedSender<RuntimeEvent>,
) -> Result<(), String> {
    let execution_id = start.execution_id;
    let state = Rc::new(RefCell::new(ExecutionState {
        execution_id,
        event_tx,
        pending_tools: HashMap::new(),
        pending_timeouts: HashMap::new(),
        next_tool_id: 1,
        next_timeout_id: 1,
    }));
    install_native_functions(ctx, &state)?;
    let run_cell = ctx
        .eval::<Function<'js>, _>(BOOTSTRAP)
        .catch(ctx)
        .map_err(|error| format!("failed to evaluate embedded QuickJS bootstrap: {error}"))?;
    remove_native_globals(ctx)?;

    let tools = serde_json::to_string(&start.tools)
        .map_err(|error| format!("failed to encode QuickJS tool metadata: {error}"))?;
    let stored = serde_json::to_string(&start.stored)
        .map_err(|error| format!("failed to encode QuickJS stored values: {error}"))?;
    let promise = run_cell
        .call::<_, Promise<'js>>((start.source, tools, stored))
        .catch(ctx)
        .map_err(|error| format!("embedded QuickJS execution failed to start: {error}"))?;
    drain_jobs(ctx);

    let result = (|| {
        loop {
            if let Some(terminal) = completed_terminal(ctx, &promise)? {
                return send_terminal(&state, terminal);
            }
            match receive_command(command_rx, next_timeout_wait(&state)) {
                CommandPoll::Command(HostCommand::ToolResult {
                    execution_id: result_execution_id,
                    id,
                    value,
                    success,
                }) if result_execution_id == execution_id => {
                    resolve_tool(ctx, &state, id, &value, success)?;
                    drain_jobs(ctx);
                }
                CommandPoll::TimerReady => {
                    if let Some(id) = next_due_timeout(&state) {
                        invoke_timeout(ctx, &state, id)?;
                        drain_jobs(ctx);
                    }
                }
                CommandPoll::Command(HostCommand::Shutdown) | CommandPoll::Disconnected => {
                    return Err("embedded QuickJS host stopped".into());
                }
                CommandPoll::Command(HostCommand::Start(_) | HostCommand::ToolResult { .. }) => {}
            }
        }
    })();
    let mut state = state.borrow_mut();
    state.pending_tools.clear();
    state.pending_timeouts.clear();
    result
}

#[allow(clippy::too_many_lines)]
fn install_native_functions<'js>(
    ctx: &Ctx<'js>,
    state: &Rc<RefCell<ExecutionState>>,
) -> Result<(), String> {
    let globals = ctx.globals();

    let tool_state = Rc::clone(state);
    globals
        .set(
            "__nanocodexTool",
            Func::from(
                move |ctx: Ctx<'js>,
                      name: String,
                      input_json: String|
                      -> rquickjs::Result<Promise<'js>> {
                    let input = serde_json::from_str(&input_json).map_err(|error| {
                        Exception::throw_type(&ctx, &format!("invalid tool input: {error}"))
                    })?;
                    let (promise, resolve, reject) = Promise::new(&ctx)?;
                    let mut state = tool_state.borrow_mut();
                    let id = state.next_tool_id;
                    state.next_tool_id = state.next_tool_id.saturating_add(1);
                    state.pending_tools.insert(
                        id,
                        (
                            Persistent::save(&ctx, resolve),
                            Persistent::save(&ctx, reject),
                        ),
                    );
                    let _ = state.event_tx.send(RuntimeEvent::ToolCall {
                        cell_id: state.execution_id,
                        id,
                        name,
                        input,
                    });
                    Ok(promise)
                },
            ),
        )
        .catch(ctx)
        .map_err(|error| format!("failed to install QuickJS tool callback: {error}"))?;

    let content_state = Rc::clone(state);
    globals
        .set(
            "__nanocodexContent",
            Func::from(
                move |ctx: Ctx<'js>, content_json: String| -> rquickjs::Result<()> {
                    let content = serde_json::from_str(&content_json).map_err(|error| {
                        Exception::throw_type(&ctx, &format!("invalid output content: {error}"))
                    })?;
                    let state = content_state.borrow();
                    let _ = state.event_tx.send(RuntimeEvent::Content {
                        cell_id: state.execution_id,
                        content,
                    });
                    Ok(())
                },
            ),
        )
        .catch(ctx)
        .map_err(|error| format!("failed to install QuickJS content callback: {error}"))?;

    let notify_state = Rc::clone(state);
    globals
        .set(
            "__nanocodexNotify",
            Func::from(move |text: String| {
                let state = notify_state.borrow();
                let _ = state.event_tx.send(RuntimeEvent::Notify {
                    cell_id: state.execution_id,
                    text,
                });
            }),
        )
        .catch(ctx)
        .map_err(|error| format!("failed to install QuickJS notify callback: {error}"))?;

    let yield_state = Rc::clone(state);
    globals
        .set(
            "__nanocodexYield",
            Func::from(move || {
                let state = yield_state.borrow();
                let _ = state.event_tx.send(RuntimeEvent::Yielded {
                    cell_id: state.execution_id,
                });
            }),
        )
        .catch(ctx)
        .map_err(|error| format!("failed to install QuickJS yield callback: {error}"))?;

    let timeout_state = Rc::clone(state);
    globals
        .set(
            "__nanocodexSetTimeout",
            Func::from(
                move |ctx: Ctx<'js>,
                      callback: Function<'js>,
                      delay_ms: i64|
                      -> rquickjs::Result<u32> {
                    let delay_ms = u64::try_from(delay_ms).unwrap_or_default();
                    let mut state = timeout_state.borrow_mut();
                    let id = state.next_timeout_id;
                    state.next_timeout_id = state.next_timeout_id.saturating_add(1);
                    state.pending_timeouts.insert(
                        id,
                        PendingTimeout {
                            callback: Persistent::save(&ctx, callback),
                            deadline: Instant::now().checked_add(Duration::from_millis(delay_ms)),
                        },
                    );
                    Ok(id)
                },
            ),
        )
        .catch(ctx)
        .map_err(|error| format!("failed to install QuickJS timer callback: {error}"))?;

    let clear_timeout_state = Rc::clone(state);
    globals
        .set(
            "__nanocodexClearTimeout",
            Func::from(move |id: u32| {
                clear_timeout_state
                    .borrow_mut()
                    .pending_timeouts
                    .remove(&id);
            }),
        )
        .catch(ctx)
        .map_err(|error| format!("failed to install QuickJS timer cleanup: {error}"))?;
    Ok(())
}

fn remove_native_globals(ctx: &Ctx<'_>) -> Result<(), String> {
    let globals = ctx.globals();
    for name in [
        "__nanocodexTool",
        "__nanocodexContent",
        "__nanocodexNotify",
        "__nanocodexYield",
        "__nanocodexSetTimeout",
        "__nanocodexClearTimeout",
        "console",
        "Atomics",
        "SharedArrayBuffer",
        "WebAssembly",
    ] {
        globals
            .remove(name)
            .catch(ctx)
            .map_err(|error| format!("failed to remove QuickJS global `{name}`: {error}"))?;
    }
    Ok(())
}

fn completed_terminal(
    ctx: &Ctx<'_>,
    promise: &Promise<'_>,
) -> Result<Option<ExecutionTerminal>, String> {
    match promise.state() {
        PromiseState::Pending => Ok(None),
        PromiseState::Resolved => {
            let encoded = promise
                .result::<String>()
                .ok_or_else(|| "embedded QuickJS promise lost its result".to_owned())?
                .catch(ctx)
                .map_err(|error| format!("failed to read embedded QuickJS result: {error}"))?;
            serde_json::from_str(&encoded)
                .map(Some)
                .map_err(|error| format!("embedded QuickJS returned an invalid result: {error}"))
        }
        PromiseState::Rejected => {
            let result = promise
                .result::<String>()
                .ok_or_else(|| "embedded QuickJS promise lost its rejection".to_owned())?
                .catch(ctx);
            match result {
                Err(error) => Err(format!("embedded QuickJS execution rejected: {error}")),
                Ok(_) => {
                    Err("rejected embedded QuickJS promise returned a successful value".to_owned())
                }
            }
        }
    }
}

fn send_terminal(
    state: &Rc<RefCell<ExecutionState>>,
    terminal: ExecutionTerminal,
) -> Result<(), String> {
    let state = state.borrow();
    let event = match terminal {
        ExecutionTerminal::Done { stored } => RuntimeEvent::Done {
            cell_id: state.execution_id,
            stored,
        },
        ExecutionTerminal::Error { message, stored } => RuntimeEvent::Error {
            cell_id: state.execution_id,
            message,
            stored,
        },
    };
    state
        .event_tx
        .send(event)
        .map_err(|_| "embedded QuickJS execution observer closed".to_owned())
}

fn resolve_tool(
    ctx: &Ctx<'_>,
    state: &Rc<RefCell<ExecutionState>>,
    id: u64,
    value: &Value,
    success: bool,
) -> Result<(), String> {
    let (resolve, reject) = state
        .borrow_mut()
        .pending_tools
        .remove(&id)
        .ok_or_else(|| format!("embedded QuickJS received a result for unknown tool call {id}"))?;
    let function = if success { resolve } else { reject };
    let function = function
        .restore(ctx)
        .map_err(|error| format!("failed to restore QuickJS tool promise: {error}"))?;
    let encoded = serde_json::to_string(value)
        .map_err(|error| format!("failed to encode QuickJS tool result: {error}"))?;
    function
        .call::<_, ()>((encoded,))
        .catch(ctx)
        .map_err(|error| format!("failed to settle QuickJS tool promise: {error}"))
}

fn invoke_timeout(
    ctx: &Ctx<'_>,
    state: &Rc<RefCell<ExecutionState>>,
    id: u32,
) -> Result<(), String> {
    let timeout = state.borrow_mut().pending_timeouts.remove(&id);
    let Some(timeout) = timeout else {
        return Ok(());
    };
    let callback = timeout
        .callback
        .restore(ctx)
        .map_err(|error| format!("failed to restore QuickJS timeout callback: {error}"))?;
    callback
        .call::<_, ()>(())
        .catch(ctx)
        .map_err(|error| format!("embedded QuickJS timeout callback failed: {error}"))
}

fn next_timeout_wait(state: &Rc<RefCell<ExecutionState>>) -> Option<Duration> {
    let now = Instant::now();
    state
        .borrow()
        .pending_timeouts
        .values()
        .filter_map(|timeout| timeout.deadline)
        .min()
        .map(|deadline| deadline.saturating_duration_since(now))
}

fn next_due_timeout(state: &Rc<RefCell<ExecutionState>>) -> Option<u32> {
    let now = Instant::now();
    state
        .borrow()
        .pending_timeouts
        .iter()
        .filter_map(|(id, timeout)| {
            timeout
                .deadline
                .filter(|deadline| *deadline <= now)
                .map(|deadline| (*id, deadline))
        })
        .min_by_key(|(id, deadline)| (*deadline, *id))
        .map(|(id, _)| id)
}

fn receive_command(
    receiver: &std_mpsc::Receiver<HostCommand>,
    timeout: Option<Duration>,
) -> CommandPoll {
    let Some(timeout) = timeout else {
        return receiver
            .recv()
            .map_or(CommandPoll::Disconnected, CommandPoll::Command);
    };
    match receiver.recv_timeout(timeout) {
        Ok(command) => CommandPoll::Command(command),
        Err(std_mpsc::RecvTimeoutError::Timeout) => CommandPoll::TimerReady,
        Err(std_mpsc::RecvTimeoutError::Disconnected) => CommandPoll::Disconnected,
    }
}

fn drain_jobs(ctx: &Ctx<'_>) {
    while ctx.execute_pending_job() {}
}
