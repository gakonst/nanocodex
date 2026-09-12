use crate::{Error, Result, engine::Engine, runtime_app_state as app_state};
use rquickjs::{
    Context, Ctx, Exception, Function, Module, Object, Persistent, Promise, Runtime,
    Value as JsValue,
    function::Constructor,
    prelude::{Func, Opt, This},
};
#[cfg(test)]
#[path = "runtime_cua_setup_tests.rs"]
mod cua_setup_tests;
#[cfg(test)]
#[path = "runtime_drain_tests.rs"]
mod drain_tests;
#[cfg(test)]
#[path = "runtime_execution_tests.rs"]
mod execution_tests;
#[path = "runtime_control.rs"]
mod provider_control;
pub use provider_control::{ExecutionValidity, ProviderControl, ProviderSuspension};
#[path = "runtime_drain.rs"]
mod drain;
pub(crate) use drain::Proof as DrainProof;
#[path = "runtime_helpers.rs"]
mod helpers;
#[path = "runtime_kernel.rs"]
mod kernel;
#[path = "runtime_microtasks.rs"]
mod microtasks;
#[path = "runtime_modules.rs"]
mod modules;
#[path = "runtime_quickjs_drain.rs"]
mod quickjs_drain;
#[path = "runtime_quickjs_tasks.rs"]
mod quickjs_tasks;
#[path = "runtime_rpc.rs"]
mod rpc;
#[path = "runtime_tasks.rs"]
mod tasks;
#[path = "runtime_timer_queue.rs"]
mod timer_queue;
use helpers::{helper_response, image_data_url, parse_url, read_image_file};
#[cfg(feature = "v8")]
#[path = "runtime_v8/mod.rs"]
mod v8backend;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

type Dispatch = Rc<RefCell<Box<dyn FnMut(&str, &Value, ProviderControl) -> Result<Value>>>>;
type PendingRpc = Rc<RefCell<rpc::Queue<Persistent<Function<'static>>>>>;
type Timers = Rc<RefCell<BTreeMap<u32, Timer>>>;
struct Timer {
    immediate: bool,
    when: Instant,
    function: Persistent<Function<'static>>,
    context: tasks::Context,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeBackend {
    #[default]
    Quickjs,
    V8,
}
impl RuntimeBackend {
    pub fn available() -> Vec<Self> {
        [Self::Quickjs, Self::V8]
            .into_iter()
            .filter(|backend| *backend == Self::Quickjs || cfg!(feature = "v8"))
            .collect()
    }
    pub fn require_available(self) -> Result<()> {
        if Self::available().contains(&self) {
            Ok(())
        } else {
            Err(Error::unsupported(
                "V8 runtime requires the v8 Cargo feature",
            ))
        }
    }
}
impl std::str::FromStr for RuntimeBackend {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self> {
        match value {
            "quickjs" => Ok(Self::Quickjs),
            "v8" => Ok(Self::V8),
            _ => Err(Error::invalid("Runtime must be quickjs or v8")),
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostOptions {
    #[serde(default)]
    pub runtime: RuntimeBackend,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub request_meta: Option<Value>,
    /// Maximum wall time awaiting one explicit MCP client approval (default five minutes).
    #[serde(default)]
    pub elicitation_timeout_ms: Option<u64>,
}
impl HostOptions {
    fn snapshot(&self) -> Value {
        let env = self
            .env
            .iter()
            .filter(|(key, _)| key.as_str() == "TINYSKY_ALT_INITIALIZE_DOCS")
            .collect::<BTreeMap<_, _>>();
        // Only this existing trusted launcher input selects the CUA adapters.
        // Keep it separate from both public env and the private docs env.
        let cua_surfaces = self.env.get("CUA_REPL_ENABLED_SURFACES");
        let request_meta = self
            .request_meta
            .as_ref()
            .filter(|v| v.is_object())
            .map(|v| {
                let mut out = serde_json::Map::new();
                if let Some(policy) = v.get("openai/confirmation_policies") {
                    out.insert("openai/confirmation_policies".into(), policy.clone());
                }
                if let Some(turn) = v.get("x-codex-turn-metadata") {
                    out.insert("x-codex-turn-metadata".into(), turn.clone());
                }
                Value::Object(out)
            });
        json!({"cwd":std::env::current_dir().ok().map(|p|p.to_string_lossy().into_owned()),"homeDir":std::env::var("HOME").ok(),"tmpDir":std::env::temp_dir().to_string_lossy(),"env":env,"cuaEnabledSurfaces":cua_surfaces,"requestMeta":request_meta})
    }
}
pub struct Host {
    backend: Backend,
}
enum Backend {
    Quickjs(QuickJsHost),
    #[cfg(feature = "v8")]
    V8(v8backend::Host),
}
impl Host {
    pub fn new(engine: Rc<RefCell<Engine>>) -> Result<Self> {
        Self::with_controlled_dispatch(
            move |method, args, control| {
                engine
                    .borrow_mut()
                    .execute_from_js_controlled(method, args, &control)
            },
            Arc::new(AtomicBool::new(false)),
            HostOptions::default(),
        )
    }
    pub fn with_dispatch(
        dispatch: impl FnMut(&str, &Value) -> Result<Value> + 'static,
        cancellation: Arc<AtomicBool>,
    ) -> Result<Self> {
        Self::with_dispatch_options(dispatch, cancellation, HostOptions::default())
    }
    pub fn with_dispatch_options(
        dispatch: impl FnMut(&str, &Value) -> Result<Value> + 'static,
        cancellation: Arc<AtomicBool>,
        options: HostOptions,
    ) -> Result<Self> {
        let mut dispatch = dispatch;
        Self::with_controlled_dispatch(
            move |method, args, _control| dispatch(method, args),
            cancellation,
            options,
        )
    }
    /// Trusted dispatch with a per-call human approval deadline capability.
    pub fn with_controlled_dispatch(
        dispatch: impl FnMut(&str, &Value, ProviderControl) -> Result<Value> + 'static,
        cancellation: Arc<AtomicBool>,
        options: HostOptions,
    ) -> Result<Self> {
        let backend = match options.runtime {
            RuntimeBackend::Quickjs => Backend::Quickjs(QuickJsHost::with_dispatch_options(
                dispatch,
                cancellation,
                options,
            )?),
            RuntimeBackend::V8 => {
                #[cfg(feature = "v8")]
                {
                    Backend::V8(v8backend::Host::with_dispatch_options(
                        dispatch,
                        cancellation,
                        options,
                    )?)
                }
                #[cfg(not(feature = "v8"))]
                {
                    return Err(Error::unsupported(
                        "V8 runtime requires the v8 Cargo feature",
                    ));
                }
            }
        };
        Ok(Self { backend })
    }
    /// Rust child transport observer; never accepts authority from JavaScript metadata.
    pub(crate) fn set_timeout_observer(
        &mut self,
        observer: impl Fn(bool) -> Result<()> + Send + Sync + 'static,
    ) -> Result<()> {
        match &mut self.backend {
            #[cfg(feature = "v8")]
            Backend::V8(host) => {
                host.set_timeout_observer(observer);
                Ok(())
            }
            Backend::Quickjs(_) => {
                let _ = observer;
                Err(Error::unsupported("Parent timeout observer requires V8"))
            }
        }
    }
    pub fn evaluate(&mut self, code: &str, timeout: Duration) -> Result<Value> {
        self.evaluate_mode(code, timeout, true)
    }
    /// CUA exposes writes and images only; never inspect the last expression.
    pub fn evaluate_without_completion(&mut self, code: &str, timeout: Duration) -> Result<Value> {
        self.evaluate_mode(code, timeout, false)
    }
    pub(crate) fn evaluate_mode(
        &mut self,
        code: &str,
        timeout: Duration,
        completion: bool,
    ) -> Result<Value> {
        match &mut self.backend {
            Backend::Quickjs(host) => host.evaluate(code, timeout, completion),
            #[cfg(feature = "v8")]
            Backend::V8(host) => host.evaluate(code, timeout, completion),
        }
    }
    /// Run due background tasks with no active tool authority or output sink.
    /// Embedders call this between cells; Worker schedules it automatically.
    pub fn tick(&mut self, budget: Duration) -> Result<bool> {
        match &mut self.backend {
            Backend::Quickjs(host) => host.tick(budget),
            #[cfg(feature = "v8")]
            Backend::V8(host) => host.tick(budget),
        }
    }
    pub fn has_background(&self) -> bool {
        match &self.backend {
            Backend::Quickjs(host) => host.has_background(),
            #[cfg(feature = "v8")]
            Backend::V8(host) => host.has_background(),
        }
    }
    pub fn set_request_meta(&mut self, value: Option<Value>) -> Result<()> {
        match &mut self.backend {
            Backend::Quickjs(host) => host.set_request_meta(value),
            #[cfg(feature = "v8")]
            Backend::V8(host) => host.set_request_meta(value),
        }
    }
    pub fn cancel(&self) {
        match &self.backend {
            Backend::Quickjs(host) => host.cancel(),
            #[cfg(feature = "v8")]
            Backend::V8(host) => host.cancel(),
        }
    }
    pub fn clear_cancel(&self) {
        match &self.backend {
            Backend::Quickjs(host) => host.clear_cancel(),
            #[cfg(feature = "v8")]
            Backend::V8(host) => host.clear_cancel(),
        }
    }
    pub fn interrupted(&self) -> bool {
        match &self.backend {
            Backend::Quickjs(host) => host.interrupted(),
            #[cfg(feature = "v8")]
            Backend::V8(host) => host.interrupted(),
        }
    }
    pub fn memory_usage(&self) -> String {
        match &self.backend {
            Backend::Quickjs(host) => host.memory_usage(),
            #[cfg(feature = "v8")]
            Backend::V8(host) => host.memory_usage(),
        }
    }
}
// These genuine builtin handles and their receiver never enter the JS bridge.
// All retained keys remain charged to the existing QuickJS managed heap.
struct AppInstructionKeys {
    set: Persistent<Object<'static>>,
    has: Persistent<Function<'static>>,
    add: Persistent<Function<'static>>,
}
impl AppInstructionKeys {
    fn new(ctx: &Ctx<'_>) -> rquickjs::Result<Self> {
        let constructor: Constructor = ctx.globals().get("Set")?;
        let prototype: Object = constructor.get("prototype")?;
        let has: Function = prototype.get("has")?;
        let add: Function = prototype.get("add")?;
        let set: Object = constructor.construct(())?;
        Ok(Self {
            set: Persistent::save(ctx, set),
            has: Persistent::save(ctx, has),
            add: Persistent::save(ctx, add),
        })
    }
    fn mark_new(
        &self,
        ctx: &Ctx<'_>,
        key: &str,
        validate: impl FnOnce() -> rquickjs::Result<()>,
    ) -> rquickjs::Result<bool> {
        let set = self.set.clone().restore(ctx)?;
        let key = rquickjs::String::from_str(ctx.clone(), key)?;
        let has = self.has.clone().restore(ctx)?;
        if has.call::<_, bool>((This(set.clone()), key.clone()))? {
            return Ok(false);
        }
        let add = self.add.clone().restore(ctx)?;
        // Argument conversion/lookup is complete before the current-clock probe.
        validate()?;
        add.call::<_, JsValue>((This(set), key))?;
        Ok(true)
    }
}
#[cfg(test)]
#[test]
fn native_app_state_quickjs_cache_checks_current_clock_before_insertion() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        let keys = AppInstructionKeys::new(&ctx).unwrap();
        let expired = Mutex::new(Instant::now());
        let suspended = Mutex::new((0, None));
        let cancelled = AtomicBool::new(false);
        let denied = keys.mark_new(&ctx, "owned-clock-key", || {
            validate_quickjs_execution(&expired, &suspended, &cancelled)
                .map_err(|error| Exception::throw_message(&ctx, &error.message))
        });
        assert!(denied.is_err());
        let _ = ctx.catch();
        // This is an isolated cache/backend unit, not a resumed public cell.
        // A distinct valid clock proves the denied operation did not insert.
        let valid = Mutex::new(Instant::now() + Duration::from_secs(5));
        assert!(
            keys.mark_new(&ctx, "owned-clock-key", || {
                validate_quickjs_execution(&valid, &suspended, &cancelled)
                    .map_err(|error| Exception::throw_message(&ctx, &error.message))
            })
            .unwrap()
        );
        assert!(
            !keys
                .mark_new(&ctx, "owned-clock-key", || {
                    panic!("already present key requires no insertion")
                })
                .unwrap()
        );
    });
}
struct QuickJsHost {
    drain: Rc<quickjs_drain::Bridge>,
    pending_rpc: PendingRpc,
    dispatch: Dispatch,
    timers: Timers,
    microtasks: Rc<microtasks::Queue>,
    timer_ready: RefCell<timer_queue::Ready>,
    tasks: Rc<tasks::Tasks>,
    rejections: quickjs_tasks::Rejections,
    app_state: Cell<app_state::Mode>,
    instruction_keys: Option<AppInstructionKeys>,
    context: Context,
    runtime: Runtime,
    deadline: Arc<Mutex<Instant>>,
    outputs: Rc<RefCell<Vec<Value>>>,
    cancellation: Arc<AtomicBool>,
    active: Rc<Cell<bool>>,
    cell_id: Rc<Cell<u64>>,
    writes: Rc<Cell<usize>>,
    suspended: Arc<Mutex<(u32, Option<Instant>)>>,
    response_meta: Rc<RefCell<serde_json::Map<String, Value>>>,
    binding_salt: String,
    request_meta: Rc<RefCell<Value>>,
}
// This probe reads the same current native state as provider suspension and
// interruption. The provider's separate lifetime binds it to one admitted call.
// No timeout is copied or extended, and suspended execution is not actionable.
fn validate_quickjs_execution(
    deadline: &Mutex<Instant>,
    suspended: &Mutex<(u32, Option<Instant>)>,
    cancelled: &AtomicBool,
) -> Result<()> {
    let ended = || Error::new(-32800, "Evaluation cancelled or timed out");
    if cancelled.load(Ordering::Acquire) {
        return Err(ended());
    }
    {
        // Preserve the established suspension -> deadline lock order. Read the
        // clock after both locks; a pre-lock timestamp could admit expiry.
        let state = suspended.lock().unwrap();
        let limit = deadline.lock().unwrap();
        if state.0 != 0 || Instant::now() >= *limit {
            return Err(ended());
        }
    }
    if cancelled.load(Ordering::Acquire) {
        return Err(ended());
    }
    Ok(())
}
impl QuickJsHost {
    pub fn with_dispatch_options(
        dispatch: impl FnMut(&str, &Value, ProviderControl) -> Result<Value> + 'static,
        cancellation: Arc<AtomicBool>,
        options: HostOptions,
    ) -> Result<Self> {
        let dispatcher: Dispatch = Rc::new(RefCell::new(Box::new(dispatch)));
        let pending_rpc = Rc::new(RefCell::new(rpc::Queue::default()));
        let enqueue_rpc = pending_rpc.clone();
        let runtime = Runtime::new().map_err(js_error)?;
        runtime.set_memory_limit(128 * 1024 * 1024);
        runtime.set_max_stack_size(1024 * 1024);
        runtime.set_loader(
            modules::Resolver,
            modules::loader().with_module("skyre:kernel", kernel::MODULE),
        );
        let mut random = [0u8; 12];
        getrandom::fill(&mut random)
            .map_err(|_| Error::action("Cannot initialize binding identity"))?;
        let binding_salt = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let deadline = Arc::new(Mutex::new(Instant::now() + Duration::from_secs(30)));
        let limit = deadline.clone();
        let cancelled = cancellation.clone();
        let suspended = Arc::new(Mutex::new((0u32, None::<Instant>)));
        let interrupt_suspended = suspended.clone();
        runtime.set_interrupt_handler(Some(Box::new(move || {
            cancelled.load(Ordering::Acquire)
                || (interrupt_suspended.lock().unwrap().0 == 0
                    && Instant::now() >= *limit.lock().unwrap())
        })));
        let context = Context::full(&runtime).map_err(js_error)?;
        // Capture before any submitted code; no auxiliary evaluator or globals.
        let instruction_keys = context
            .with(|ctx| AppInstructionKeys::new(&ctx))
            .map_err(js_error)?;
        let timers: Timers = Rc::new(RefCell::new(BTreeMap::new()));
        let create_timer = timers.clone();
        let remove_timer = timers.clone();
        let refresh_timer = timers.clone();
        let microtasks = Rc::new(microtasks::Queue::default());
        let next_timer = Rc::new(Cell::new(0u32));
        let event_clock_origin = Instant::now();
        let outputs = Rc::new(RefCell::new(Vec::new()));
        let writer = outputs.clone();
        let active = Rc::new(Cell::new(false));
        let cell_id = Rc::new(Cell::new(0u64));
        let is_active = active.clone();
        let write_active = active.clone();
        let tasks = Rc::new(tasks::Tasks::default());
        let rejections =
            quickjs_tasks::install(&runtime, &context, tasks.clone()).map_err(js_error)?;
        let drain = Rc::new(quickjs_drain::Bridge::default());
        let rpc_drain = Rc::downgrade(&drain);
        let origin_active = tasks.clone();
        let origin_active_cell = cell_id.clone();
        let origin_cell = tasks.clone();
        let origin_metadata = tasks.clone();
        let origin_write = tasks.clone();
        let write_cell = cell_id.clone();
        let origin_rpc = tasks.clone();
        let rpc_active = active.clone();
        let rpc_cell = cell_id.clone();
        let timer_context = tasks.clone();
        let writes = Rc::new(Cell::new(0usize));
        let write_count = writes.clone();
        let suspend = suspended.clone();
        let suspend_deadline = deadline.clone();
        let suspend_active = active.clone();
        let suspend_origin = tasks.clone();
        let suspend_cell = cell_id.clone();
        let response_meta = Rc::new(RefCell::new(serde_json::Map::new()));
        let meta_writer = response_meta.clone();
        let meta_active = active.clone();
        let meta_origin = tasks.clone();
        let meta_cell = cell_id.clone();
        let files = RefCell::new(modules::Files::default());
        let codecs = RefCell::new(modules::Codecs::default());
        let request_meta = Rc::new(RefCell::new(options.snapshot()["requestMeta"].clone()));

        context
            .with(|ctx| -> rquickjs::Result<()> {
                drain.install(&ctx, tasks.clone())?;
                ctx.globals().set("__skyre_host_options", options.snapshot().to_string())?;
                ctx.globals().set("__skyre_request_meta",Func::from(move ||origin_metadata.current.borrow().metadata.to_string()))?;
                ctx.globals().set("__skyre_runtime_platform", if cfg!(target_os="macos") {"mac"} else if cfg!(target_os="windows") {"windows"} else {"linux"})?;
                ctx.globals().set("__skyre_cell_active", Func::from(move || is_active.get() && !origin_active.poisoned.get() && origin_active.current.borrow().id==origin_active_cell.get()))?;
                ctx.globals().set("__skyre_cell_id", Func::from(move || origin_cell.current.borrow().id as f64))?;
                ctx.globals().set("__skyre_suspend_timeout", Func::from(move |ctx:Ctx<'_>, start:bool| -> rquickjs::Result<()> {
                    if !suspend_active.get() || suspend_origin.poisoned.get() || suspend_origin.current.borrow().id != suspend_cell.get() { return Err(Exception::throw_message(&ctx,"node_repl exec context not found")); }
                    let mut state=suspend.lock().unwrap();
                    if start {
                        if state.0 == 0 {state.1=Some(Instant::now());}
                        state.0=state.0.checked_add(1).ok_or_else(||Exception::throw_message(&ctx,"Timeout suspension depth exceeded"))?;
                    } else if state.0 > 0 {
                        state.0-=1;
                        if state.0 == 0 {
                            let elapsed=state.1.take().unwrap().elapsed();
                            let mut limit=suspend_deadline.lock().unwrap();
                            *limit=limit.checked_add(elapsed).ok_or_else(||Exception::throw_message(&ctx,"Timeout suspension exceeded the clock range"))?;
                        }
                    }
                    Ok(())
                }))?;
                ctx.globals().set("__skyre_response_meta", Func::from(move |ctx:Ctx<'_>, text:String| -> rquickjs::Result<()> {
                    if !meta_active.get() || meta_origin.poisoned.get() || meta_origin.current.borrow().id != meta_cell.get() {return Err(Exception::throw_message(&ctx,"node_repl exec context not found"));}
                    let value:Value=serde_json::from_str(&text).map_err(|_|Exception::throw_type(&ctx,"Response metadata must be an object"))?;
                    let object=value.as_object().ok_or_else(||Exception::throw_type(&ctx,"Response metadata must be an object"))?;
                    let mut merged=meta_writer.borrow().clone();
                    merged.extend(object.clone());
                    if serde_json::to_vec(&merged).map_or(true,|v|v.len()>65536) {return Err(Exception::throw_message(&ctx,"Response metadata exceeds 64 KiB"));}
                    *meta_writer.borrow_mut()=merged; Ok(())
                }))?;
                ctx.globals().set("__skyre_url_parse", Func::from(move |request:String| -> String { helper_response(parse_url(&request)) }))?;
                ctx.globals().set("__skyre_read_image_file", Func::from(move |url:String| -> String { helper_response(read_image_file(&url)) }))?;
                ctx.globals().set("__skyre_image_data_url", Func::from(move |url:String| -> String { helper_response(image_data_url(&url)) }))?;
                ctx.globals().set("__skyre_fs",Func::from(move |request:String|files.borrow_mut().call(&request)))?;
                ctx.globals().set("__skyre_codec",Func::from(move |request:String|codecs.borrow_mut().call(&request)))?;
                ctx.globals().set(
                    "__skyre_rpc",
                    Func::from(move |ctx: Ctx<'_>, method: String, args: String, suspend: Opt<bool>| -> rquickjs::Result<Persistent<Promise<'static>>> {
                        if !rpc_active.get() || origin_rpc.poisoned.get() || origin_rpc.current.borrow().id != rpc_cell.get() {
                            return Err(Exception::throw_message(&ctx, "node_repl exec context not found"));
                        }
                        let (promise, resolve, _) = Promise::new(&ctx)?;
                        let bridge = rpc_drain.upgrade().ok_or_else(||Exception::throw_message(&ctx,"Native drain owner ended"))?;
                        let request = bridge.registry.borrow_mut().request().map_err(|e|Exception::throw_message(&ctx,&e.message))?;
                        bridge.tag(promise.as_value().clone(), request);
                        enqueue_rpc.borrow_mut().push(method, args, Persistent::save(&ctx, resolve), origin_rpc.current.borrow().clone(), suspend.0.unwrap_or(false), request)
                            .map_err(|error|Exception::throw_message(&ctx,&error.message))?;
                        Ok(Persistent::save(&ctx, promise))
                    }),
                )?;
                ctx.globals().set(
                    "__skyre_write",
                    Func::from(
                        move |ctx: Ctx<'_>,
                              text: String,
                              channel: String,
                              kind: Opt<String>|
                              -> rquickjs::Result<()> {
                            let mut output = writer.borrow_mut();
                            if !write_active.get() || origin_write.poisoned.get() || origin_write.current.borrow().id!=write_cell.get() { if kind.0.as_deref()==Some("line"){return Ok(());}return Err(Exception::throw_message(&ctx,"node_repl exec context not found")); }
                            if write_count.get() >= 256
                                || text.len()
                                    + output
                                        .iter()
                                        .map(|v: &Value| v.to_string().len())
                                        .sum::<usize>()
                                    > 4 * 1024 * 1024
                            {
                                return Err(Exception::throw_message(
                                    &ctx,
                                    "Cell output budget exceeded (256 items / 4 MiB)",
                                ));
                            }
                            write_count.set(write_count.get() + 1);
                            let value = serde_json::from_str::<Value>(&text).unwrap_or(json!(text));
                            let kind = kind.0.unwrap_or_else(|| if channel == "image" {"image"} else if channel == "output" {"write"} else {"named"}.into());
                            let named = kind == "named";
                            if kind != "image" && value.is_string()
                                && let Some(previous) = output.iter_mut().find(|v|v["channel"] == channel && v["named"] == named && v["value"].is_string())
                            {
                                previous["value"] = json!(format!("{}{}",previous["value"].as_str().unwrap(),value.as_str().unwrap()));
                                previous["kind"] = json!(kind);
                            } else {
                                output.push(json!({"channel":channel,"value":value,"named":named,"kind":kind}));
                            }
                            Ok(())
                        },
                    ),
                )?;
                ctx.globals().set("__skyre_event_now", Func::from(move || event_clock_origin.elapsed().as_secs_f64() * 1000.0))?;
                ctx.globals().set("__skyre_timer_schedule",Func::from(move |function:Function<'_>,delay:f64,kind:Opt<u8>|->rquickjs::Result<u32>{
                    let ctx=function.ctx().clone();
                    if !delay.is_finite()||!(0.0..=2147483647.0).contains(&delay)||create_timer.borrow().len()>=1024{return Err(Exception::throw_message(&ctx,"Invalid timeout or too many timers"));}
                    let id=next_timer.get().checked_add(1).ok_or_else(||Exception::throw_message(&ctx,"Timer ID exhausted"))?;next_timer.set(id);
                    create_timer.borrow_mut().insert(id,Timer{immediate:kind.0==Some(1),when:Instant::now()+Duration::from_secs_f64(delay/1000.0),function:Persistent::save(&ctx,function),context:timer_context.current.borrow().clone()});Ok(id)
                }))?;
                ctx.globals().set("__skyre_timer_clear",Func::from(move|id:u32|{remove_timer.borrow_mut().remove(&id);}))?;
                ctx.globals().set("__skyre_timer_refresh",Func::from(move|id:u32,delay:f64|->bool {
                    if !delay.is_finite()||!(0.0..=2147483647.0).contains(&delay){return false;}
                    if let Some(timer)=refresh_timer.borrow_mut().get_mut(&id){timer.when=Instant::now()+Duration::from_secs_f64(delay/1000.0);true}else{false}
                }))?;
                microtasks::install(&ctx,microtasks.clone(),tasks.clone())?;
                ctx.eval::<(), _>(include_str!("node_repl.js"))?;
                crate::runtime_url_search_params::install(&ctx)?;
                ctx.eval::<(), _>(include_str!("runtime_modules.js"))?;
                ctx.eval::<(), _>(include_str!("runtime_timers.js"))?;
                ctx.eval::<(), _>(include_str!("browser_facade.js"))?;
                ctx.eval::<(), _>(include_str!("sky_facade.js"))?;
                ctx.eval::<(), _>(include_str!("cua_docs.js"))?;
                ctx.eval::<(), _>(include_str!("facade.js"))?;
                Ok(())
            })
            .map_err(js_error)?;
        Ok(Self {
            drain,
            pending_rpc,
            dispatch: dispatcher,
            timers,
            microtasks,
            timer_ready: Default::default(),
            tasks,
            rejections,
            app_state: Cell::new(app_state::Mode::default()),
            instruction_keys: Some(instruction_keys),
            context,
            runtime,
            deadline,
            outputs,
            cancellation,
            active,
            cell_id,
            writes,
            suspended,
            response_meta,
            binding_salt,
            request_meta,
        })
    }
    pub fn evaluate(&mut self, code: &str, timeout: Duration, completion: bool) -> Result<Value> {
        if code.len() > 1024 * 1024 {
            return Err(Error::invalid("JavaScript cell exceeds 1 MiB"));
        }
        *self.deadline.lock().unwrap() = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| Error::invalid("Evaluation timeout exceeds the clock range"))?;
        self.outputs.borrow_mut().clear();
        self.response_meta.borrow_mut().clear();
        *self.suspended.lock().unwrap() = (0, None);
        self.writes.set(0);
        self.cell_id.set(self.cell_id.get().saturating_add(1));
        self.drain.finish();
        self.drain.registry.borrow_mut().begin(self.cell_id.get());
        self.active.set(true);
        self.tasks.current.replace(tasks::Context {
            id: self.cell_id.get(),
            metadata: Rc::from(self.request_meta.borrow().to_string()),
        });
        let mut execution_ms = None;
        let mut exception_message = None;
        let result = self.context.with(|ctx| {
            let evaluated = (|| -> rquickjs::Result<JsValue> {
                let metadata =
                    ctx.eval::<String, _>("JSON.stringify(__skyreKernelInternals.metadata())")?;
                let bindings: Vec<(String, String)> = serde_json::from_str(&metadata)
                    .map_err(|_| Exception::throw_message(&ctx, "Invalid persistent bindings"))?;
                let compiled =
                    kernel::compile(code, &bindings, self.cell_id.get(), &self.binding_salt)
                        .map_err(|error| Exception::throw_syntax(&ctx, &error.message))?;
                let module = Module::declare(
                    ctx.clone(),
                    format!("skyre:cell/{}/{}", self.binding_salt, self.cell_id.get()),
                    compiled.source,
                )?;
                let started = Instant::now();
                let evaluation = (|| {
                    let (module, promise) = module.eval()?;
                    self.settle_promise(&ctx, promise)?;
                    Ok::<_, rquickjs::Error>(module)
                })();
                execution_ms = Some(rpc::elapsed_ms(started));
                let module = evaluation?;
                self.drain.registry.borrow_mut().settled();
                let drain = ctx.eval::<Promise, _>("__skyreDrainOutput()")?;
                self.settle_promise(&ctx, drain)?;
                module.get(&compiled.result_name)
            })();
            match evaluated {
                Ok(value) => {
                    if !completion {
                        return Ok(Value::Null);
                    }
                    let value = ctx
                        .json_stringify(value)
                        .map_err(js_error)?
                        .map(|s| s.to_string().map_err(js_error))
                        .transpose()?;
                    Ok(value
                        .and_then(|v| serde_json::from_str::<Value>(&v).ok())
                        .unwrap_or(Value::Null))
                }
                Err(error) => {
                    let mut exception = ctx.catch();
                    if !self.interrupted() {
                        let drained = ctx
                            .eval::<Promise, _>("__skyreDrainOutput()")
                            .and_then(|promise| self.settle_promise(&ctx, promise));
                        if drained.is_err() {
                            let drain_exception = ctx.catch();
                            if self.interrupted() {
                                exception = drain_exception;
                            }
                        }
                    }
                    let object = exception.as_object();
                    let detail = object
                        .and_then(|o| o.get::<_, String>("message").ok())
                        .or_else(|| exception.as_string().and_then(|s| s.to_string().ok()))
                        .unwrap_or_else(|| error.to_string());
                    exception_message = Some(detail.clone());
                    let stack = object
                        .and_then(|o| o.get::<_, String>("stack").ok())
                        .unwrap_or_default();
                    let message = if stack.is_empty() || stack.contains(&detail) {
                        if stack.is_empty() { detail } else { stack }
                    } else {
                        format!("{detail}\n{stack}")
                    };
                    let code = object
                        .and_then(|o| o.get::<_, i32>("code").ok())
                        .unwrap_or(-32004);
                    let _ = ctx.eval::<(), _>(format!(
                        "try{{__skyreKernelInternals.finish(false,{})}}catch{{}}",
                        self.cell_id.get()
                    ));
                    Err(Error::new(code, message))
                }
            }
        });
        if self.interrupted() {
            self.timers.borrow_mut().clear();
            self.microtasks.clear();
            self.pending_rpc.borrow_mut().clear();
        }
        if self.tasks.poisoned.get() {
            self.outputs.borrow_mut().clear();
        }
        self.drain.finish();
        self.active.set(false);
        if !self.cancellation.load(Ordering::Acquire)
            && self.suspended.lock().unwrap().0 == 0
            && Instant::now() >= *self.deadline.lock().unwrap()
        {
            self.outputs.borrow_mut().clear();
            return Ok(tasks::timeout_result());
        }
        let outputs = self
            .outputs
            .borrow_mut()
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
        let mut response = match result {
            Ok(value) => {
                json!({"value":value,"outputs":outputs,"responseMeta":*self.response_meta.borrow()})
            }
            Err(error) => {
                json!({"error":error,"exceptionMessage":exception_message,"outputs":outputs,"responseMeta":*self.response_meta.borrow()})
            }
        };
        if let Some(duration) = execution_ms {
            response["executionDurationMs"] = json!(duration);
        }
        Ok(response)
    }

    fn check_reply_execution(&self, ctx: &Ctx<'_>) -> rquickjs::Result<()> {
        validate_quickjs_execution(&self.deadline, &self.suspended, &self.cancellation)
            .map_err(|error| Exception::throw_message(ctx, &error.message))
    }
    fn app_state_response(
        &self,
        ctx: &Ctx<'_>,
        method: &str,
        input: &Value,
        result: Result<Value>,
    ) -> rquickjs::Result<String> {
        let mut mode = self.app_state.get();
        mode.observe(method, &result);
        self.app_state.set(mode);
        // Ordinary provider errors retain their exact existing integer codes.
        let value = match result {
            Ok(value) => value,
            Err(error) => return Ok(helper_response(Err(error))),
        };
        let requested = match mode.request(method, input) {
            Ok(Some(requested)) => requested,
            Ok(None) => return Ok(helper_response(Ok(value))),
            Err(error) => return Ok(helper_response(Err(error))),
        };
        let plan = match app_state::Plan::new(requested, value) {
            Ok(plan) => plan,
            Err(message) => return Ok(app_state::validation_response(message)),
        };
        let prepend = match plan.key() {
            Some(key) => self
                .instruction_keys
                .as_ref()
                .ok_or_else(|| Exception::throw_message(ctx, "App-state owner ended"))?
                .mark_new(ctx, key, || self.check_reply_execution(ctx))?,
            None => false,
        };
        let formatted = plan.finish(prepend);
        self.check_reply_execution(ctx)?;
        Ok(helper_response(formatted))
    }
    fn dispatch_queued_rpc(&self, ctx: &Ctx<'_>) -> rquickjs::Result<bool> {
        let Some(call) = self.pending_rpc.borrow_mut().pop() else {
            return Ok(false);
        };
        if !self.active.get() || self.tasks.poisoned.get() || call.context.id != self.cell_id.get()
        {
            return Err(Exception::throw_message(
                ctx,
                "node_repl exec context not found",
            ));
        }
        let activation_model =
            crate::browser_activation::Model::from_task_metadata(&call.context.metadata);
        let _context = self.tasks.scope(call.context);
        let deadline = self.deadline.clone();
        let suspended = self.suspended.clone();
        let cancelled = self.cancellation.clone();
        let execution_deadline = self.deadline.clone();
        let execution_suspended = self.suspended.clone();
        let execution_cancelled = self.cancellation.clone();
        let control = ProviderControl::new_with_activation_model(
            move |start| {
                let mut state = suspended.lock().unwrap();
                let mut limit = deadline.lock().unwrap();
                if start {
                    if cancelled.load(Ordering::Acquire)
                        || (state.0 == 0 && Instant::now() >= *limit)
                    {
                        return Err(Error::new(-32800, "Evaluation cancelled or timed out"));
                    }
                    if state.0 == 0 {
                        state.1 = Some(Instant::now());
                    }
                    state.0 = state
                        .0
                        .checked_add(1)
                        .ok_or_else(|| Error::action("Timeout suspension depth exceeded"))?;
                } else if state.0 > 0 {
                    state.0 -= 1;
                    if state.0 == 0 {
                        let began = state.1.take().unwrap();
                        *limit = limit.checked_add(began.elapsed()).ok_or_else(|| {
                            Error::action("Timeout suspension exceeded the clock range")
                        })?;
                    }
                }
                Ok(())
            },
            Some(Arc::new(move || {
                validate_quickjs_execution(
                    &execution_deadline,
                    &execution_suspended,
                    &execution_cancelled,
                )
            })),
            activation_model,
        );
        if self.pending_rpc.borrow().is_empty()
            // settle_promise just observed execute_pending_job() == false.
            && self.microtasks.is_empty()
            // Any future timer may become runnable inside the slice. Without
            // native ownership/deadline proof for it, keep all that time charged.
            && self.timers.borrow().is_empty()
        {
            control
                .set_drain_proof(self.drain.registry.borrow().proof(call.request))
                .map_err(|e| Exception::throw_message(ctx, &e.message))?;
        }
        let lifetime = control.lifetime();
        let result = if self.tasks.poisoned.get() {
            Err(Error::action("Asynchronous context nesting exceeded"))
        } else {
            let suspended = if call.suspend {
                Some(control.suspend())
            } else {
                None
            };
            match suspended {
                Some(Err(error)) => Err(error),
                guard => {
                    let result =
                        self.dispatch.borrow_mut()(&call.method, &call.input, control.clone());
                    guard
                        .map(|guard| guard.and_then(|guard| guard.resume().map(|_| ())))
                        .transpose()
                        .and(result)
                }
            }
        };
        self.drain
            .registry
            .borrow_mut()
            .replied(call.request, control.continuation());
        self.drain.collect();
        lifetime
            .finish()
            .map_err(|error| Exception::throw_message(ctx, &error.message))?;
        if self.interrupted() {
            return Err(Exception::throw_message(
                ctx,
                "Evaluation cancelled or timed out",
            ));
        }
        self.check_reply_execution(ctx)?;
        let encoded = self.app_state_response(ctx, &call.method, &call.input, result)?;
        self.check_reply_execution(ctx)?;
        let response = rquickjs::String::from_str(ctx.clone(), &encoded)?;
        drop(encoded);
        let resolver = call.resolver.restore(ctx)?;
        self.check_reply_execution(ctx)?;
        resolver.call::<_, ()>((response,))?;
        Ok(true)
    }
    fn settle_promise<'js>(
        &self,
        ctx: &Ctx<'js>,
        promise: Promise<'js>,
    ) -> rquickjs::Result<JsValue<'js>> {
        loop {
            if self.cancellation.load(Ordering::Acquire)
                || (self.suspended.lock().unwrap().0 == 0
                    && Instant::now() >= *self.deadline.lock().unwrap())
            {
                return Err(Exception::throw_message(
                    ctx,
                    "Evaluation cancelled or timed out",
                ));
            }
            if let Some(failure) = self.tasks.failure.borrow().clone() {
                return Err(Exception::throw_message(ctx, &failure));
            }
            if ctx.execute_pending_job() {
                continue;
            }
            // A microtask checkpoint determines whether a rejection stayed
            // unobserved. The module result itself is consumed by this host.
            self.rejections
                .borrow_mut()
                .remove(&Persistent::save(ctx, promise.as_value().clone()));
            if let Some(rejected) = self.rejections.borrow().iter().next().cloned() {
                let value = rejected.restore(ctx)?;
                if let Some(rejected) = value.as_promise() {
                    let _ = rejected.result::<JsValue>();
                }
                let error = ctx.catch();
                let detail = error
                    .as_object()
                    .and_then(|value| value.get::<_, String>("message").ok())
                    .or_else(|| error.as_string().and_then(|value| value.to_string().ok()))
                    .unwrap_or_else(|| "Unhandled Promise rejection".into());
                self.tasks.poisoned.set(true);
                return Err(Exception::throw_message(
                    ctx,
                    &tasks::fatal("unhandled rejection", &detail),
                ));
            }
            if let Some(result) = promise.result::<JsValue>() {
                return result;
            }
            if self.dispatch_queued_rpc(ctx)? {
                continue;
            }
            let due = self.timer_ready.borrow_mut().next(
                self.timers
                    .borrow()
                    .iter()
                    .map(|(id, timer)| (*id, timer.when, timer.immediate)),
            );
            if let Some(id) = due {
                let timer = self.timers.borrow_mut().remove(&id).unwrap();
                self.tasks.enter(timer.context);
                let result = timer
                    .function
                    .restore(ctx)
                    .and_then(|function| function.call::<_, ()>(()));
                self.tasks.leave();
                if result.is_err() {
                    let error = ctx.catch();
                    let detail = error
                        .as_object()
                        .and_then(|value| value.get::<_, String>("message").ok())
                        .or_else(|| error.as_string().and_then(|value| value.to_string().ok()))
                        .unwrap_or_else(|| "Uncaught timer exception".into());
                    self.tasks.poisoned.set(true);
                    return Err(Exception::throw_message(
                        ctx,
                        &tasks::fatal("uncaught exception", &detail),
                    ));
                }
                continue;
            }
            let wait = self
                .timers
                .borrow()
                .values()
                .map(|timer| timer.when.saturating_duration_since(Instant::now()))
                .min()
                .unwrap_or(Duration::from_millis(1))
                .min(Duration::from_millis(1));
            std::thread::sleep(wait);
        }
    }
    fn has_background(&self) -> bool {
        !self.timers.borrow().is_empty() || self.runtime.is_job_pending()
    }
    fn tick(&mut self, budget: Duration) -> Result<bool> {
        if !self.has_background() {
            return Ok(false);
        }
        *self.deadline.lock().unwrap() = Instant::now()
            .checked_add(budget)
            .ok_or_else(|| Error::invalid("Background deadline exceeds clock range"))?;
        let result = self.context.with(|ctx| -> rquickjs::Result<()> {
            for _ in 0..1024 {
                if self.cancellation.load(Ordering::Acquire)
                    || Instant::now() >= *self.deadline.lock().unwrap()
                {
                    return Err(Exception::throw_message(
                        &ctx,
                        "Background execution timed out",
                    ));
                }
                if self.tasks.poisoned.get() {
                    return Err(Exception::throw_message(&ctx, "Background task failed"));
                }
                if ctx.execute_pending_job() {
                    continue;
                }
                let due = self.timer_ready.borrow_mut().next(
                    self.timers
                        .borrow()
                        .iter()
                        .map(|(id, timer)| (*id, timer.when, timer.immediate)),
                );
                let Some(id) = due else { break };
                let timer = self.timers.borrow_mut().remove(&id).unwrap();
                self.tasks.enter(timer.context);
                let result = timer
                    .function
                    .restore(&ctx)
                    .and_then(|function| function.call::<_, ()>(()));
                self.tasks.leave();
                result?;
            }
            Ok(())
        });
        if result.is_err() || !self.rejections.borrow().is_empty() {
            self.tasks.poisoned.set(true);
        }
        if self.tasks.poisoned.get() {
            return Err(Error::action("Background task failed; kernel reset"));
        }
        Ok(self.has_background())
    }
    pub fn cancel(&self) {
        self.cancellation.store(true, Ordering::Release);
    }
    /// Trusted worker/embedding operation between cells; no JavaScript setter.
    pub fn set_request_meta(&mut self, metadata: Option<Value>) -> Result<()> {
        if self.active.get() {
            return Err(Error::action(
                "Cannot replace host metadata during an active cell",
            ));
        }
        *self.request_meta.borrow_mut() = HostOptions {
            request_meta: metadata,
            ..Default::default()
        }
        .snapshot()["requestMeta"]
            .clone();
        Ok(())
    }
    pub fn clear_cancel(&self) {
        self.cancellation.store(false, Ordering::Release);
    }
    /// A hard interruption can leave queued jobs from a partially evaluated cell.
    /// The worker discards this context before accepting another cell.
    pub fn interrupted(&self) -> bool {
        self.tasks.poisoned.get()
            || self.cancellation.load(Ordering::Acquire)
            || (self.suspended.lock().unwrap().0 == 0
                && Instant::now() >= *self.deadline.lock().unwrap())
    }
    pub fn memory_usage(&self) -> String {
        format!("{:?}", self.runtime.memory_usage())
    }
}
fn js_error(error: rquickjs::Error) -> Error {
    Error::new(-32004, error.to_string())
}

impl Drop for QuickJsHost {
    fn drop(&mut self) {
        self.drain.finish();
        // Release native Persistent roots while Context/Runtime are still alive.
        self.instruction_keys.take();
        self.runtime.set_promise_hook(None);
        self.runtime.set_host_promise_rejection_tracker(None);
        self.rejections.borrow_mut().clear();
        self.timers.borrow_mut().clear();
        self.microtasks.clear();
        self.pending_rpc.borrow_mut().clear();
    }
}
