//! Persistent runtime worker; native services stay on their owning thread.
//! V8 runs in an owned Rust child because its fatal OOM paths abort the process.
//! Cancellation is cooperative around provider calls and preemptive for JS CPU.
use crate::{
    Error, Result,
    runtime::{Host, HostOptions, ProviderControl},
};
use serde_json::Value;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
pub(crate) enum Command {
    Eval {
        ticket: u64,
        code: String,
        timeout: Duration,
        completion: bool,
    },
    Metadata {
        value: Option<Value>,
        reply: SyncSender<Result<()>>,
    },
    Reset,
    Stop,
}
pub enum Event {
    Call {
        method: String,
        args: Value,
        reply: SyncSender<Result<Value>>,
        control: ProviderControl,
    },
    Done {
        ticket: u64,
        result: Result<Value>,
    },
}
pub struct Worker {
    commands: Sender<Command>,
    events: Receiver<Event>,
    cancel: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    next: u64,
    active: bool,
    kernel_reset: Arc<AtomicBool>,
    child_pid: Arc<AtomicU32>,
}
impl Worker {
    pub fn new() -> Self {
        Self::with_options(HostOptions::default())
    }
    pub fn with_options(options: HostOptions) -> Self {
        Self::with_options_and_executable(options, std::env::current_exe().unwrap_or_default())
    }
    /// The executable must implement `worker::run_child`; the CLI uses itself.
    /// Library embedders supply their trusted `skyre` binary here.
    pub fn with_options_and_executable(
        mut options: HostOptions,
        executable: std::path::PathBuf,
    ) -> Self {
        let (commands, rx) = mpsc::channel();
        let (send, events) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = cancel.clone();
        let kernel_reset = Arc::new(AtomicBool::new(false));
        let lost = kernel_reset.clone();
        let child_pid = Arc::new(AtomicU32::new(0));
        let pid = child_pid.clone();
        let join = thread::spawn(move || {
            if options.runtime == crate::runtime::RuntimeBackend::V8 {
                crate::worker_process::supervise(
                    executable, options, rx, send, cancelled, lost, pid,
                );
                return;
            }
            let mut host: Option<Host> = None;
            loop {
                let incoming = if host.as_ref().is_some_and(Host::has_background) {
                    rx.recv_timeout(Duration::from_millis(10))
                } else {
                    rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected)
                };
                let command = match incoming {
                    Ok(command) => command,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if let Some(current) = host.as_mut()
                            && current.has_background()
                            && (current.tick(Duration::from_millis(100)).is_err()
                                || current.interrupted())
                        {
                            lost.store(true, Ordering::Release);
                            host = None;
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
                        let result = (|| {
                            if host.is_none() {
                                let output = send.clone();
                                let interrupted = cancelled.clone();
                                host = Some(Host::with_controlled_dispatch(
                                    move |method, args, control| {
                                        if interrupted.load(Ordering::Acquire) {
                                            return Err(Error::new(-32800, "Evaluation cancelled"));
                                        }
                                        let (reply, receive) = mpsc::sync_channel(1);
                                        output
                                            .send(Event::Call {
                                                method: method.into(),
                                                args: args.clone(),
                                                reply,
                                                control,
                                            })
                                            .map_err(|_| {
                                                Error::action("Service owner disconnected")
                                            })?;
                                        loop {
                                            match receive.recv_timeout(Duration::from_millis(20)) {
                                                Ok(result) => return result,
                                                Err(mpsc::RecvTimeoutError::Timeout) => {
                                                    if interrupted.load(Ordering::Acquire) {
                                                        return Err(Error::new(
                                                            -32800,
                                                            "Evaluation cancelled",
                                                        ));
                                                    }
                                                }
                                                Err(_) => {
                                                    return Err(Error::action(
                                                        "Service reply disconnected",
                                                    ));
                                                }
                                            }
                                        }
                                    },
                                    cancelled.clone(),
                                    options.clone(),
                                )?);
                            }
                            host.as_mut()
                                .unwrap()
                                .evaluate_mode(&code, timeout, completion)
                        })();
                        if host.as_ref().is_some_and(Host::interrupted) {
                            lost.store(true, Ordering::Release);
                            host = None;
                        }
                        if send.send(Event::Done { ticket, result }).is_err() {
                            break;
                        }
                    }
                    Command::Metadata { value, reply } => {
                        let result = if let Some(host) = host.as_mut() {
                            host.set_request_meta(value.clone())
                        } else {
                            Ok(())
                        };
                        if result.is_ok() {
                            options.request_meta = value;
                        }
                        let _ = reply.send(result);
                    }
                    Command::Reset => host = None,
                    Command::Stop => break,
                }
            }
        });
        Self {
            commands,
            events,
            cancel,
            join: Some(join),
            next: 0,
            active: false,
            kernel_reset,
            child_pid,
        }
    }
    pub fn start(&mut self, code: &str, timeout: Duration) -> Result<u64> {
        self.start_mode(code, timeout, true)
    }
    /// Public CUA output never observes implicit completion values.
    pub fn start_without_completion(&mut self, code: &str, timeout: Duration) -> Result<u64> {
        self.start_mode(code, timeout, false)
    }
    fn start_mode(&mut self, code: &str, timeout: Duration, completion: bool) -> Result<u64> {
        if self.active {
            return Err(Error::action("An evaluation is already active"));
        }
        self.cancel.store(false, Ordering::Release);
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| Error::action("Worker ticket overflow"))?;
        self.commands
            .send(Command::Eval {
                ticket: self.next,
                code: code.into(),
                timeout,
                completion,
            })
            .map_err(|_| Error::action("Runtime worker terminated"))?;
        self.active = true;
        Ok(self.next)
    }
    pub fn event(&mut self, timeout: Duration) -> Result<Option<Event>> {
        match self.events.recv_timeout(timeout) {
            Ok(event) => {
                if matches!(&event, Event::Done { .. }) {
                    self.active = false;
                }
                Ok(Some(event))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(_) => Err(Error::action("Runtime worker disconnected")),
        }
    }
    pub fn set_request_meta(&mut self, value: Option<Value>) -> Result<()> {
        if self.active {
            return Err(Error::action(
                "Cannot replace trusted metadata during an active cell",
            ));
        }
        let (reply, receive) = mpsc::sync_channel(1);
        self.commands
            .send(Command::Metadata { value, reply })
            .map_err(|_| Error::action("Runtime worker terminated"))?;
        receive
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| Error::action("Runtime metadata update was not acknowledged"))?
    }
    /// Host-only diagnostic identity of this worker's currently owned Rust child.
    pub fn runtime_child_pid(&self) -> Option<u32> {
        let pid = self.child_pid.load(Ordering::Acquire);
        (pid != 0).then_some(pid)
    }
    /// Drain after Event::Done to release parent resources owned by the lost kernel.
    /// Inspect pending kernel loss without consuming cleanup responsibility.
    pub fn kernel_reset_pending(&self) -> bool {
        self.kernel_reset.load(Ordering::Acquire)
    }
    pub fn take_kernel_reset(&self) -> bool {
        self.kernel_reset.swap(false, Ordering::AcqRel)
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }
    pub fn reset(&self) -> Result<()> {
        if self.active {
            return Err(Error::action(
                "Cancel and drain the active cell before reset",
            ));
        }
        self.commands
            .send(Command::Reset)
            .map_err(|_| Error::action("Runtime worker terminated"))
    }
}
impl Default for Worker {
    fn default() -> Self {
        Self::new()
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel();
        let _ = self.commands.send(Command::Stop);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Entry point for the private Rust runtime child protocol.
pub fn run_child() -> Result<()> {
    crate::worker_process::run_child()
}
