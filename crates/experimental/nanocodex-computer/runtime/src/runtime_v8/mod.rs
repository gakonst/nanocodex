//! In-process V8 backend behind the same persistent Rust Host/Worker contract.
use super::{Dispatch, HostOptions, ProviderControl, helpers, kernel, modules, rpc};
use crate::{Error, Result, runtime_app_state as app_state};
mod callbacks;
mod imports;
mod memory;
mod tasks;
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::Rc,
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
static PLATFORM: OnceLock<v8::SharedRef<v8::Platform>> = OnceLock::new();
fn platform() -> &'static v8::SharedRef<v8::Platform> {
    PLATFORM.get_or_init(|| {
        let p = v8::new_default_platform(2, false).make_shared();
        v8::V8::initialize_platform(p.clone());
        v8::V8::initialize();
        p
    })
}
#[derive(Default)]
struct Clock {
    deadline: Option<Instant>,
    suspended: u32,
    suspended_at: Option<Instant>,
    stop: bool,
}
struct Control {
    clock: Mutex<Clock>,
    wake: Condvar,
    cancel: Arc<AtomicBool>,
    reason: AtomicU8,
}
impl Control {
    fn validate_execution(&self) -> Result<()> {
        let ended = || Error::new(-32800, "Evaluation cancelled or timed out");
        if self.cancel.load(Ordering::Acquire) || self.reason.load(Ordering::Acquire) != 0 {
            return Err(ended());
        }
        {
            let clock = self.clock.lock().unwrap();
            // The live native clock is authoritative, including prior explicit
            // suspension/resume. This probe never changes or credits that clock.
            if clock.stop
                || clock.suspended != 0
                || clock
                    .deadline
                    .is_none_or(|deadline| Instant::now() >= deadline)
            {
                return Err(ended());
            }
        }
        if self.cancel.load(Ordering::Acquire) || self.reason.load(Ordering::Acquire) != 0 {
            return Err(ended());
        }
        Ok(())
    }
}
struct State {
    drain: super::drain::Registry,
    pending_rpc: rpc::Queue<v8::Global<v8::PromiseResolver>>,
    exception_message: Option<String>,
    app_state: app_state::Mode,
    instruction_keys: Option<Rc<v8::Global<v8::Set>>>,
    timeout_observer: Option<Arc<dyn Fn(bool) -> Result<()> + Send + Sync>>,
    active: bool,
    cell: u64,
    request_meta: Value,
    outputs: Vec<Value>,
    writes: usize,
    meta: serde_json::Map<String, Value>,
    timers: BTreeMap<u32, Timer>,
    event_clock_origin: Instant,
    timer_ready: super::timer_queue::Ready,
    microtasks: BTreeMap<u32, Microtask>,
    next_microtask: u32,
    tasks: Rc<super::tasks::Tasks>,
    promise_key: Option<v8::Global<v8::Private>>,
    rejections: Vec<v8::Global<v8::Promise>>,
    next_timer: u32,
    modules: BTreeMap<String, v8::Global<v8::Module>>,
    files: modules::Files,
    codecs: modules::Codecs,
    dispatch: Dispatch,
    control: Arc<Control>,
}
struct Microtask {
    function: v8::Global<v8::Function>,
    context: super::tasks::Context,
}
struct Timer {
    immediate: bool,
    when: Instant,
    function: v8::Global<v8::Function>,
    context: super::tasks::Context,
}
pub struct Host {
    context: Option<v8::Global<v8::Context>>,
    isolate: Option<v8::OwnedIsolate>,
    state: Rc<RefCell<State>>,
    control: Arc<Control>,
    watchdog: Option<thread::JoinHandle<()>>,
    heap: Box<memory::Heap>,
    buffer_memory: Arc<memory::Buffers>,
    dispatch: Dispatch,
    cancel: Arc<AtomicBool>,
    salt: String,
    heap_limit: usize,
    buffer_limit: usize,
    poisoned: bool,
    options: HostOptions,
}
// Guard the entered-isolate stack through all Result returns and Rust unwinding.
struct Entry<'a>(&'a mut v8::OwnedIsolate);
impl<'a> Entry<'a> {
    fn new(isolate: &'a mut v8::OwnedIsolate) -> Self {
        unsafe {
            isolate.enter();
        }
        Self(isolate)
    }
}
impl std::ops::Deref for Entry<'_> {
    type Target = v8::OwnedIsolate;
    fn deref(&self) -> &Self::Target {
        self.0
    }
}
impl std::ops::DerefMut for Entry<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0
    }
}
impl Drop for Entry<'_> {
    fn drop(&mut self) {
        unsafe {
            self.0.exit();
        }
    }
}
impl Host {
    pub fn with_dispatch_options(
        dispatch: impl FnMut(&str, &Value, ProviderControl) -> Result<Value> + 'static,
        cancel: Arc<AtomicBool>,
        options: HostOptions,
    ) -> Result<Self> {
        Self::construct(
            Rc::new(RefCell::new(Box::new(dispatch))),
            cancel,
            128 << 20,
            64 << 20,
            options,
        )
    }
    fn construct(
        dispatch: Dispatch,
        cancel: Arc<AtomicBool>,
        heap_limit: usize,
        buffer_limit: usize,
        options: HostOptions,
    ) -> Result<Self> {
        if heap_limit < 8 << 20 || buffer_limit < 1 << 20 {
            return Err(Error::invalid(
                "V8 probe minimum heap/buffer limits are 8/1 MiB",
            ));
        }
        platform();
        let control = Arc::new(Control {
            clock: Mutex::new(Clock::default()),
            wake: Condvar::new(),
            cancel: cancel.clone(),
            reason: AtomicU8::new(0),
        });
        let buffer_memory = memory::Buffers::new(buffer_limit, control.clone());
        let params = v8::CreateParams::default()
            .heap_limits(0, heap_limit)
            .array_buffer_allocator(memory::allocator(&buffer_memory));
        let mut isolate = v8::Isolate::new(params);
        isolate.set_allow_atomics_wait(false);
        isolate.set_microtasks_policy(v8::MicrotasksPolicy::Explicit);
        isolate.set_host_import_module_dynamically_callback(imports::dynamic);
        let handle = isolate.thread_safe_handle();
        *buffer_memory.handle.lock().unwrap() = Some(handle.clone());
        let mut heap = Box::new(memory::Heap {
            handle: handle.clone(),
            control: control.clone(),
            calls: 0,
            ceiling: 0,
        });
        isolate.add_near_heap_limit_callback(
            memory::near_limit,
            (&mut *heap as *mut memory::Heap).cast(),
        );
        let state = Rc::new(RefCell::new(State {
            drain: Default::default(),
            pending_rpc: Default::default(),
            exception_message: None,
            app_state: app_state::Mode::default(),
            instruction_keys: None,
            timeout_observer: None,
            active: false,
            cell: 0,
            request_meta: options.snapshot()["requestMeta"].clone(),
            outputs: vec![],
            writes: 0,
            meta: Default::default(),
            timers: BTreeMap::new(),
            event_clock_origin: Instant::now(),
            timer_ready: Default::default(),
            microtasks: BTreeMap::new(),
            next_microtask: 0,
            tasks: Rc::new(super::tasks::Tasks::default()),
            promise_key: None,
            rejections: vec![],
            next_timer: 0,
            modules: BTreeMap::new(),
            files: Default::default(),
            codecs: Default::default(),
            dispatch: dispatch.clone(),
            control: control.clone(),
        }));
        isolate.set_slot(state.clone());
        isolate.set_promise_hook(tasks::promise_hook);
        isolate.set_promise_reject_callback(tasks::rejected);
        let context = {
            v8::scope!(let scope,&mut isolate);
            let context = v8::Context::new(scope, Default::default());
            let scope = &mut v8::ContextScope::new(scope, context);
            let key = v8::Private::new(scope, None);
            state.borrow_mut().promise_key = Some(v8::Global::new(scope, key));
            callbacks::install(scope, &options)?;
            v8::Global::new(scope, context)
        };
        let mut random = [0u8; 12];
        getrandom::fill(&mut random).map_err(|_| Error::action("Binding identity unavailable"))?;
        let salt = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let watch = control.clone();
        let watchdog = thread::spawn(move || {
            let mut clock = watch.clock.lock().unwrap();
            while !clock.stop {
                // Idle route workers consume no polling CPU. Evaluation and
                // shutdown notify this condition; active cancellation still polls.
                if clock.deadline.is_none() {
                    clock = watch.wake.wait(clock).unwrap();
                    continue;
                }
                if clock.deadline.is_some()
                    && (watch.cancel.load(Ordering::Acquire)
                        || (clock.suspended == 0 && Instant::now() >= clock.deadline.unwrap()))
                {
                    watch
                        .reason
                        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                        .ok();
                    handle.terminate_execution();
                    clock.deadline = None;
                }
                clock = watch
                    .wake
                    .wait_timeout(clock, Duration::from_millis(1))
                    .unwrap()
                    .0;
            }
        });
        // OwnedIsolate enters on creation. Keep idle hosts exited so independent
        // Host values may be evaluated and dropped in any order on this thread.
        unsafe {
            isolate.exit();
        }
        Ok(Self {
            context: Some(context),
            isolate: Some(isolate),
            state,
            control,
            watchdog: Some(watchdog),
            heap,
            buffer_memory,
            dispatch,
            cancel,
            salt,
            heap_limit,
            buffer_limit,
            poisoned: false,
            options,
        })
    }
    pub fn reset(&mut self) -> Result<()> {
        let metadata = self.state.borrow().request_meta.clone();
        self.shutdown();
        self.poisoned = true;
        self.cancel.store(false, Ordering::Release);
        let mut fresh = Self::construct(
            self.dispatch.clone(),
            self.cancel.clone(),
            self.heap_limit,
            self.buffer_limit,
            self.options.clone(),
        )?;
        fresh.set_request_meta(Some(metadata))?;
        *self = fresh;
        Ok(())
    }
    /// Only trusted Rust host code can replace current tool metadata between cells.
    pub fn set_request_meta(&mut self, metadata: Option<Value>) -> Result<()> {
        let mut state = self.state.borrow_mut();
        if state.active {
            return Err(Error::action(
                "Cannot replace host metadata during an active cell",
            ));
        }
        state.request_meta = metadata
            .and_then(|value| {
                value.as_object().map(|object| {
                    Value::Object(
                        object
                            .iter()
                            .filter(|(key, _)| {
                                ["openai/confirmation_policies", "x-codex-turn-metadata"]
                                    .contains(&key.as_str())
                            })
                            .map(|(key, value)| (key.clone(), value.clone()))
                            .collect(),
                    )
                })
            })
            .unwrap_or(Value::Null);
        Ok(())
    }
    pub fn set_timeout_observer(
        &mut self,
        observer: impl Fn(bool) -> Result<()> + Send + Sync + 'static,
    ) {
        self.state.borrow_mut().timeout_observer = Some(Arc::new(observer));
    }
    pub fn evaluate(&mut self, code: &str, timeout: Duration, completion: bool) -> Result<Value> {
        if self.poisoned {
            self.reset()?;
        }
        if code.len() > 1024 * 1024 {
            return Err(Error::invalid("JavaScript cell exceeds 1 MiB"));
        }
        let started = Instant::now();
        let deadline = started
            .checked_add(timeout)
            .ok_or_else(|| Error::invalid("Evaluation timeout exceeds clock range"))?;
        {
            let mut state = self.state.borrow_mut();
            state.cell += 1;
            let cell = state.cell;
            state.drain.begin(cell);
            state.active = true;
            state.outputs.clear();
            state.exception_message = None;
            state.writes = 0;
            state.meta.clear();
        }
        {
            let mut clock = self.control.clock.lock().unwrap();
            clock.deadline = Some(deadline);
            clock.suspended = 0;
            clock.suspended_at = None;
        }
        self.control.wake.notify_all();
        let cell = self.state.borrow().cell;
        self.state
            .borrow()
            .tasks
            .current
            .replace(super::tasks::Context {
                id: cell,
                metadata: Rc::from(self.state.borrow().request_meta.to_string()),
            });
        let state = self.state.clone();
        let mut execution_ms = None;
        let mut entered = Entry::new(self.isolate.as_mut().unwrap());
        let result = {
            v8::scope!(let scope,&mut *entered);
            let context = v8::Local::new(scope, self.context.as_ref().unwrap());
            let scope = &mut v8::ContextScope::new(scope, context);
            v8::tc_scope!(let scope,scope);
            let mut result = (|| {
                let metadata = eval(scope, "JSON.stringify(__skyreKernelInternals.metadata())")
                    .ok_or_else(|| exception(scope))?
                    .to_rust_string_lossy(scope);
                let bindings: Vec<(String, String)> = serde_json::from_str(&metadata)?;
                let compiled = kernel::compile(code, &bindings, cell, &self.salt)?;
                let module = imports::compile(
                    scope,
                    &format!("skyre:cell/{}/{cell}", self.salt),
                    &compiled.source,
                )
                .ok_or_else(|| exception(scope))?;
                if module.instantiate_module(scope, imports::resolve) != Some(true) {
                    return Err(exception(scope));
                }
                let code_started = Instant::now();
                let evaluated = (|| {
                    let promise: v8::Local<v8::Promise> = module
                        .evaluate(scope)
                        .ok_or_else(|| exception(scope))?
                        .try_into()
                        .map_err(|_| Error::action("V8 module did not return Promise"))?;
                    settle_promise(scope, promise, &state, &self.control)
                })();
                execution_ms = Some(rpc::elapsed_ms(code_started));
                evaluated?;
                state.borrow_mut().drain.settled();
                let drain = eval(scope, "__skyreDrainOutput()").ok_or_else(|| exception(scope))?;
                let drain = v8::Local::<v8::Promise>::try_from(drain)
                    .map_err(|_| Error::action("Output drain did not return Promise"))?;
                settle_promise(scope, drain, &state, &self.control)?;
                if !completion {
                    return Ok(Value::Null);
                }
                let namespace = module
                    .get_module_namespace()
                    .to_object(scope)
                    .ok_or_else(|| exception(scope))?;
                let key = v8::String::new(scope, &compiled.result_name)
                    .ok_or_else(|| exception(scope))?;
                let value = namespace
                    .get(scope, key.into())
                    .ok_or_else(|| exception(scope))?;
                let json = v8::json::stringify(scope, value)
                    .map(|value| value.to_rust_string_lossy(scope));
                if scope.has_caught() {
                    return Err(exception(scope));
                }
                let value = json
                    .and_then(|text| serde_json::from_str(&text).ok())
                    .unwrap_or(Value::Null);
                check_execution(scope, &self.control)?;
                Ok(value)
            })();
            if result.is_err()
                && !scope.is_execution_terminating()
                && !state.borrow().tasks.poisoned.get()
            {
                let original_message = state.borrow_mut().exception_message.take();
                scope.reset();
                let drained = eval(scope, "__skyreDrainOutput()")
                    .ok_or_else(|| exception(scope))
                    .and_then(|value| {
                        v8::Local::<v8::Promise>::try_from(value)
                            .map_err(|_| Error::action("Output drain did not return Promise"))
                    })
                    .and_then(|promise| settle_promise(scope, promise, &state, &self.control));
                if scope.is_execution_terminating()
                    || self.control.reason.load(Ordering::Acquire) != 0
                    || state.borrow().tasks.poisoned.get()
                {
                    if let Err(error) = drained {
                        result = Err(error);
                    }
                } else {
                    // A provider rejection does not replace the submitted-code
                    // error; fatal asynchronous failures and timeouts do.
                    state.borrow_mut().exception_message = original_message;
                }
            }
            if result.is_err() && !scope.is_execution_terminating() {
                scope.reset();
                let _ = eval(
                    scope,
                    &format!("try{{__skyreKernelInternals.finish(false,{cell})}}catch{{}}"),
                );
            }
            result
        };
        drop(entered);
        {
            let mut clock = self.control.clock.lock().unwrap();
            clock.deadline = None;
            clock.suspended = 0;
            clock.suspended_at = None;
        }
        let reason = self.control.reason.load(Ordering::Acquire);
        self.poisoned = reason != 0 || self.state.borrow().tasks.poisoned.get();
        let mut state = self.state.borrow_mut();
        state.drain.finish();
        state.active = false;
        if self.poisoned {
            state.timers.clear();
            state.microtasks.clear();
            state.pending_rpc.clear();
        }
        if state.tasks.poisoned.get() {
            state.outputs.clear();
        }
        if reason == 1 && !self.cancel.load(Ordering::Acquire) {
            state.outputs.clear();
            return Ok(super::tasks::timeout_result());
        }
        let outputs = state
            .outputs
            .drain(..)
            .map(|mut output| {
                if output["kind"] == "line"
                    && let Some(text) = output["value"].as_str()
                {
                    output["value"] = json!(text.strip_suffix('\n').unwrap_or(text));
                }
                output
            })
            .collect::<Vec<_>>();
        let result = if reason == 1 {
            Err(Error::new(-32004, "Evaluation cancelled or timed out"))
        } else if reason == 2 || reason == 3 {
            Err(Error::new(-32004, "V8 memory budget exceeded"))
        } else {
            result
        };
        let mut response = json!({"outputs":outputs,"responseMeta":state.meta});
        if let Some(duration) = execution_ms {
            response["executionDurationMs"] = json!(duration);
        }
        match result {
            Ok(value) => response["value"] = value,
            Err(error) => {
                response["exceptionMessage"] = json!(if reason != 0 {
                    error.message.clone()
                } else {
                    state
                        .exception_message
                        .take()
                        .unwrap_or_else(|| error.message.clone())
                });
                response["error"] = json!(error);
            }
        }
        Ok(response)
    }
    pub fn has_background(&self) -> bool {
        let state = self.state.borrow();
        !state.timers.is_empty() || !state.microtasks.is_empty()
    }
    pub fn tick(&mut self, budget: Duration) -> Result<bool> {
        if !self.has_background() {
            return Ok(false);
        }
        {
            let mut clock = self.control.clock.lock().unwrap();
            clock.deadline = Some(
                Instant::now()
                    .checked_add(budget)
                    .ok_or_else(|| Error::invalid("Background deadline exceeds clock range"))?,
            );
            clock.suspended = 0;
            clock.suspended_at = None;
        }
        self.control.wake.notify_all();
        let state = self.state.clone();
        let mut entered = Entry::new(self.isolate.as_mut().unwrap());
        let result = {
            v8::scope!(let scope,&mut *entered);
            let context = v8::Local::new(scope, self.context.as_ref().unwrap());
            let scope = &mut v8::ContextScope::new(scope, context);
            v8::tc_scope!(let scope,scope);
            (|| -> Result<()> {
                for iteration in 0..=1024 {
                    check_execution(scope, &self.control)
                        .map_err(|_| Error::action("Background execution timed out"))?;
                    scope.perform_microtask_checkpoint();
                    if state.borrow().tasks.poisoned.get() {
                        return Err(Error::action("Background task failed"));
                    }
                    check_execution(scope, &self.control)
                        .map_err(|_| Error::action("Background execution timed out"))?;
                    if iteration == 1024 {
                        break;
                    }
                    let due = {
                        let mut s = state.borrow_mut();
                        let State {
                            timer_ready,
                            timers,
                            ..
                        } = &mut *s;
                        timer_ready.next(
                            timers
                                .iter()
                                .map(|(id, timer)| (*id, timer.when, timer.immediate)),
                        )
                    };
                    let Some(id) = due else { break };
                    let timer = state.borrow_mut().timers.remove(&id).unwrap();
                    let tasks = state.borrow().tasks.clone();
                    check_execution(scope, &self.control)
                        .map_err(|_| Error::action("Background execution timed out"))?;
                    tasks.enter(timer.context);
                    let function = v8::Local::new(scope, &timer.function);
                    let undefined = v8::undefined(scope);
                    let result = function.call(scope, undefined.into(), &[]);
                    tasks.leave();
                    if result.is_none() {
                        return Err(exception(scope));
                    }
                }
                Ok(())
            })()
        };
        drop(entered);
        self.control.clock.lock().unwrap().deadline = None;
        if result.is_err() || !state.borrow().rejections.is_empty() {
            state.borrow().tasks.poisoned.set(true);
            self.poisoned = true;
        }
        if self.interrupted() {
            return Err(Error::action("Background task failed; kernel reset"));
        }
        Ok(self.has_background())
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    pub fn clear_cancel(&self) {
        self.cancel.store(false, Ordering::Release);
    }
    pub fn interrupted(&self) -> bool {
        self.poisoned
            || self.state.borrow().tasks.poisoned.get()
            || self.control.reason.load(Ordering::Acquire) != 0
            || self.cancel.load(Ordering::Acquire)
    }
    pub fn memory_usage(&self) -> String {
        json!({"backend":"v8","version":v8::V8::get_version(),"arrayBufferBytes":self.buffer_memory.used.load(Ordering::Acquire),"arrayBufferLimit":self.buffer_limit,"managedHeapLimit":self.heap_limit,"heapLimitCallbacks":self.heap.calls,"poisoned":self.poisoned}).to_string()
    }
}
impl Host {
    fn shutdown(&mut self) {
        // Balance the entry consumed by OwnedIsolate::drop. All globals and host
        // slots are released while this isolate is current, before disposal.
        if let Some(isolate) = &self.isolate {
            unsafe {
                isolate.enter();
            }
        }
        self.control.clock.lock().unwrap().stop = true;
        self.control.wake.notify_all();
        if let Some(watchdog) = self.watchdog.take() {
            let _ = watchdog.join();
        }
        self.context.take();
        let instruction_keys = {
            let mut state = self.state.borrow_mut();
            state.timers.clear();
            state.microtasks.clear();
            state.pending_rpc.clear();
            state.rejections.clear();
            state.promise_key = None;
            state.modules.clear();
            state.instruction_keys.take()
        };
        // Release the cache Global while the isolate is entered, outside the
        // State borrow. Local clones exist only during synchronous formatting.
        drop(instruction_keys);
        if let Some(isolate) = &mut self.isolate {
            isolate.remove_near_heap_limit_callback(memory::near_limit, 0);
            isolate.remove_slot::<Rc<RefCell<State>>>();
        }
        self.isolate.take();
    }
}
fn eval<'s>(scope: &mut v8::PinScope<'s, '_>, source: &str) -> Option<v8::Local<'s, v8::Value>> {
    let source = v8::String::new(scope, source)?;
    v8::Script::compile(scope, source, None)?.run(scope)
}
fn property<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: Option<v8::Local<v8::Object>>,
    name: &str,
) -> Option<v8::Local<'s, v8::Value>> {
    let key = v8::String::new(scope, name)?;
    object.and_then(|object| object.get(scope, key.into()))
}
// Check the actual native clock at continuation boundaries; the watchdog may
// not have sampled an expired deadline yet. This never extends that deadline.
fn check_execution(
    scope: &mut v8::PinnedRef<'_, v8::TryCatch<'_, '_, v8::HandleScope<'_>>>,
    control: &Control,
) -> Result<()> {
    if control.validate_execution().is_err() {
        control
            .reason
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .ok();
        scope.terminate_execution();
    }
    if scope.is_execution_terminating() || control.reason.load(Ordering::Acquire) != 0 {
        Err(Error::new(-32004, "Evaluation cancelled or timed out"))
    } else {
        Ok(())
    }
}
fn app_state_response(
    scope: &mut v8::PinnedRef<'_, v8::TryCatch<'_, '_, v8::HandleScope<'_>>>,
    state: &Rc<RefCell<State>>,
    control: &Control,
    method: &str,
    input: &Value,
    result: Result<Value>,
) -> Result<String> {
    let mode = {
        let mut state = state.borrow_mut();
        state.app_state.observe(method, &result);
        state.app_state
    };
    let value = match result {
        Ok(value) => value,
        Err(error) => return Ok(helpers::helper_response(Err(error))),
    };
    let requested = match mode.request(method, input) {
        Ok(Some(requested)) => requested,
        Ok(None) => return Ok(helpers::helper_response(Ok(value))),
        Err(error) => return Ok(helpers::helper_response(Err(error))),
    };
    let plan = match app_state::Plan::new(requested, value) {
        Ok(plan) => plan,
        Err(message) => return Ok(app_state::validation_response(message)),
    };
    let prepend = if let Some(key) = plan.key() {
        // Rc cloning releases the State borrow before any engine operation.
        let owned = state.borrow().instruction_keys.clone();
        let owned = match owned {
            Some(owned) => owned,
            None => {
                let set = v8::Set::new(scope);
                let owned = Rc::new(v8::Global::new(scope, set));
                check_execution(scope, control)?;
                state.borrow_mut().instruction_keys = Some(owned.clone());
                owned
            }
        };
        let set = v8::Local::new(scope, &*owned);
        let key = v8::String::new(scope, key).ok_or_else(|| exception(scope))?;
        if set.has(scope, key.into()).ok_or_else(|| exception(scope))? {
            false
        } else {
            check_execution(scope, control)?;
            set.add(scope, key.into()).ok_or_else(|| exception(scope))?;
            true
        }
    } else {
        false
    };
    let formatted = plan.finish(prepend);
    check_execution(scope, control)?;
    Ok(helpers::helper_response(formatted))
}
fn dispatch_queued_rpc(
    scope: &mut v8::PinnedRef<'_, v8::TryCatch<'_, '_, v8::HandleScope<'_>>>,
    state: &Rc<RefCell<State>>,
    control: &Arc<Control>,
) -> Result<bool> {
    check_execution(scope, control)?;
    let Some(call) = state.borrow_mut().pending_rpc.pop() else {
        return Ok(false);
    };
    let tasks = state.borrow().tasks.clone();
    if !state.borrow().active
        || tasks.poisoned.get()
        || control.cancel.load(Ordering::Acquire)
        || control.reason.load(Ordering::Acquire) != 0
        || call.context.id != state.borrow().cell
    {
        return Err(Error::action("node_repl exec context not found"));
    }
    let activation_model =
        crate::browser_activation::Model::from_task_metadata(&call.context.metadata);
    let _context = tasks.scope(call.context);
    let runtime_control = control.clone();
    let observer = state.borrow().timeout_observer.clone();
    let execution_control = control.clone();
    let provider = ProviderControl::new_with_activation_model(
        move |start| {
            let mut clock = runtime_control.clock.lock().unwrap();
            let mut overflow = false;
            if start {
                if runtime_control.cancel.load(Ordering::Acquire)
                    || runtime_control.reason.load(Ordering::Acquire) != 0
                    || (clock.suspended == 0
                        && clock
                            .deadline
                            .is_none_or(|deadline| Instant::now() >= deadline))
                {
                    return Err(Error::new(-32800, "Evaluation cancelled or timed out"));
                }
                if clock.suspended == 0 {
                    clock.suspended_at = Some(Instant::now());
                }
                clock.suspended = clock
                    .suspended
                    .checked_add(1)
                    .ok_or_else(|| Error::action("Timeout suspension depth exceeded"))?;
            } else if clock.suspended > 0 {
                clock.suspended -= 1;
                if clock.suspended == 0
                    && let (Some(began), Some(deadline)) =
                        (clock.suspended_at.take(), clock.deadline)
                {
                    match deadline.checked_add(began.elapsed()) {
                        Some(extended) => clock.deadline = Some(extended),
                        None => overflow = true,
                    }
                }
            }
            drop(clock);
            runtime_control.wake.notify_all();
            if let Some(observer) = &observer
                && let Err(error) = observer(start)
            {
                runtime_control.cancel.store(true, Ordering::Release);
                return Err(error);
            }
            if overflow {
                Err(Error::action("Timeout suspension exceeded the clock range"))
            } else {
                Ok(())
            }
        },
        Some(Arc::new(move || execution_control.validate_execution())),
        activation_model,
    );
    {
        let state = state.borrow();
        if state.pending_rpc.is_empty() && state.microtasks.is_empty() && state.timers.is_empty() {
            provider.set_drain_proof(state.drain.proof(call.request))?;
        }
    }
    let lifetime = provider.lifetime();
    let result = if tasks.poisoned.get() {
        Err(Error::action("Asynchronous context nesting exceeded"))
    } else {
        let suspended = if call.suspend {
            Some(provider.suspend())
        } else {
            None
        };
        match suspended {
            Some(Err(error)) => Err(error),
            guard => {
                let dispatch = state.borrow().dispatch.clone();
                let result = dispatch.borrow_mut()(&call.method, &call.input, provider.clone());
                guard
                    .map(|guard| guard.and_then(|guard| guard.resume().map(|_| ())))
                    .transpose()
                    .and(result)
            }
        }
    };
    state
        .borrow_mut()
        .drain
        .replied(call.request, provider.continuation());
    lifetime.finish()?;
    check_execution(scope, control)?;
    let text = app_state_response(scope, state, control, &call.method, &call.input, result)?;
    check_execution(scope, control)?;
    let text = v8::String::new(scope, &text).ok_or_else(|| exception(scope))?;
    let resolver = v8::Local::new(scope, call.resolver);
    check_execution(scope, control)?;
    resolver
        .resolve(scope, text.into())
        .ok_or_else(|| exception(scope))?;
    Ok(true)
}
// Run the same timed Promise/timer checkpoint on success and ordinary-error
// cleanup. Ignored provider work is still owned by the original active cell.
fn settle_promise(
    scope: &mut v8::PinnedRef<'_, v8::TryCatch<'_, '_, v8::HandleScope<'_>>>,
    promise: v8::Local<v8::Promise>,
    state: &Rc<RefCell<State>>,
    control: &Arc<Control>,
) -> Result<()> {
    promise.mark_as_handled();
    loop {
        check_execution(scope, control)?;
        scope.perform_microtask_checkpoint();
        let failure = state.borrow().tasks.failure.borrow().clone();
        if let Some(message) = failure {
            state.borrow_mut().exception_message = Some(message.clone());
            return Err(Error::new(-32004, message));
        }
        check_execution(scope, control)?;
        // The host consumes the module Promise; every other
        // unobserved rejection follows the installed fatal policy.
        state.borrow_mut().rejections.retain(|previous| {
            !v8::Local::<v8::Value>::from(v8::Local::new(scope, previous))
                .strict_equals(promise.into())
        });
        let rejected = state
            .borrow()
            .rejections
            .first()
            .map(|value| v8::Local::new(scope, value));
        if let Some(rejected) = rejected {
            let reason = rejected.result(scope);
            let error = value_error(scope, reason);
            let detail = state
                .borrow_mut()
                .exception_message
                .take()
                .unwrap_or(error.message);
            let message = super::tasks::fatal("unhandled rejection", &detail);
            state.borrow_mut().exception_message = Some(message.clone());
            state.borrow().tasks.poisoned.set(true);
            return Err(Error::new(-32004, message));
        }
        match promise.state() {
            v8::PromiseState::Fulfilled => {
                check_execution(scope, control)?;
                break;
            }
            v8::PromiseState::Rejected => {
                let value = promise.result(scope);
                return Err(value_error(scope, value));
            }
            v8::PromiseState::Pending => {}
        }
        if dispatch_queued_rpc(scope, state, control)? {
            continue;
        }
        let due = {
            let mut s = state.borrow_mut();
            let State {
                timer_ready,
                timers,
                ..
            } = &mut *s;
            timer_ready.next(
                timers
                    .iter()
                    .map(|(id, timer)| (*id, timer.when, timer.immediate)),
            )
        };
        if let Some(id) = due {
            let timer = state.borrow_mut().timers.remove(&id).unwrap();
            let tasks = state.borrow().tasks.clone();
            check_execution(scope, control)?;
            tasks.enter(timer.context);
            let function = v8::Local::new(scope, &timer.function);
            let undefined = v8::undefined(scope);
            let result = function.call(scope, undefined.into(), &[]);
            tasks.leave();
            if result.is_none() {
                let error = exception(scope);
                let detail = state
                    .borrow_mut()
                    .exception_message
                    .take()
                    .unwrap_or(error.message);
                let message = super::tasks::fatal("uncaught exception", &detail);
                state.borrow_mut().exception_message = Some(message.clone());
                tasks.poisoned.set(true);
                return Err(Error::new(-32004, message));
            }
            // Every timer is a task boundary: settle its microtasks before
            // another timer or a wait, including a just-fulfilled cell.
            continue;
        }
        if v8::Platform::pump_message_loop(platform(), scope, false) {
            continue;
        }
        let sleep = state
            .borrow()
            .timers
            .values()
            .map(|timer| timer.when.saturating_duration_since(Instant::now()))
            .min()
            .unwrap_or(Duration::from_millis(1))
            .min(Duration::from_millis(1));
        thread::sleep(sleep);
    }
    Ok(())
}
fn value_error(scope: &mut v8::PinScope<'_, '_>, value: v8::Local<v8::Value>) -> Error {
    let object = value.to_object(scope);
    let detail = property(scope, object, "message")
        .filter(|v| v.is_string())
        .map(|v| v.to_rust_string_lossy(scope))
        .unwrap_or_else(|| value.to_rust_string_lossy(scope));
    let stack = property(scope, object, "stack")
        .filter(|v| v.is_string())
        .map(|v| v.to_rust_string_lossy(scope))
        .unwrap_or_default();
    let code = property(scope, object, "code")
        .filter(|v| v.is_int32())
        .and_then(|v| v.int32_value(scope))
        .unwrap_or(-32004);
    scope
        .get_slot::<Rc<RefCell<State>>>()
        .unwrap()
        .borrow_mut()
        .exception_message = Some(detail.clone());
    let message = if stack.is_empty() {
        detail
    } else if stack.contains(&detail) {
        stack
    } else {
        format!("{detail}\n{stack}")
    };
    Error::new(code, message)
}
fn exception(scope: &mut v8::PinnedRef<'_, v8::TryCatch<'_, '_, v8::HandleScope<'_>>>) -> Error {
    if scope.has_terminated() {
        Error::new(-32004, "Evaluation cancelled or timed out")
    } else {
        scope
            .exception()
            .map(|value| value_error(scope, value))
            .unwrap_or_else(|| Error::new(-32004, "JavaScript evaluation failed"))
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_v8_execution_validity_reads_current_clock_and_termination() {
        let deadline = Instant::now() + Duration::from_secs(30);
        let control = Control {
            clock: Mutex::new(Clock {
                deadline: Some(deadline),
                ..Default::default()
            }),
            wake: Condvar::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            reason: AtomicU8::new(0),
        };
        for _ in 0..4 {
            control.validate_execution().unwrap();
        }
        {
            let clock = control.clock.lock().unwrap();
            assert_eq!(clock.deadline, Some(deadline));
            assert_eq!(clock.suspended, 0);
            assert_eq!(clock.suspended_at, None);
        }
        control.clock.lock().unwrap().deadline = Some(Instant::now());
        assert!(control.validate_execution().is_err());
        control.clock.lock().unwrap().deadline = Some(deadline);
        control.validate_execution().unwrap();
        control.clock.lock().unwrap().suspended = 1;
        assert!(control.validate_execution().is_err());
        control.clock.lock().unwrap().suspended = 0;
        control.validate_execution().unwrap();
        control.clock.lock().unwrap().stop = true;
        assert!(control.validate_execution().is_err());
        control.clock.lock().unwrap().stop = false;
        control.clock.lock().unwrap().deadline = None;
        assert!(control.validate_execution().is_err());
        control.clock.lock().unwrap().deadline = Some(deadline);
        control.cancel.store(true, Ordering::Release);
        assert!(control.validate_execution().is_err());
        control.cancel.store(false, Ordering::Release);
        for reason in [1, 2] {
            control.reason.store(reason, Ordering::Release);
            assert!(control.validate_execution().is_err());
        }
        control.reason.store(0, Ordering::Release);
        control.validate_execution().unwrap();
        assert_eq!(control.clock.lock().unwrap().deadline, Some(deadline));
    }

    #[test]
    fn v8_managed_heap_and_arraybuffer_exhaustion_recover() {
        let dispatch: Dispatch = Rc::new(RefCell::new(Box::new(|_, _, _| {
            Ok(json!({"target":"mac"}))
        })));
        let mut host = Host::construct(
            dispatch,
            Arc::new(AtomicBool::new(false)),
            16 << 20,
            4 << 20,
            HostOptions::default(),
        )
        .unwrap();
        let result = host
            .evaluate(
                "let retained=[];for(;;)retained.push(new Array(10000).fill(42));",
                Duration::from_secs(5),
                true,
            )
            .unwrap();
        assert_eq!(result["exceptionMessage"], "V8 memory budget exceeded");
        assert!(host.interrupted());
        assert!(host.heap.calls > 0);
        let result = host
            .evaluate(
                "nodeRepl.write(typeof retained)",
                Duration::from_secs(5),
                true,
            )
            .unwrap();
        assert!(result.get("error").is_none());
        assert!(!host.interrupted());
        let result = host
            .evaluate(
                "let buffers=[];for(;;)buffers.push(new Uint8Array(1024*1024));",
                Duration::from_secs(5),
                true,
            )
            .unwrap();
        assert_eq!(result["exceptionMessage"], "V8 memory budget exceeded");
        assert!(host.interrupted());
        assert!(host.buffer_memory.used.load(Ordering::Acquire) <= 4 << 20);
        let result = host
            .evaluate("nodeRepl.write(42)", Duration::from_secs(5), true)
            .unwrap();
        assert!(result.get("error").is_none());
        assert!(!host.interrupted());
    }
}
