//! Hand-owned recording service. Transport clients never own the capture lifetime.
use super::{
    hand_recording_control::{self, ControlGuard, Handler},
    screen_publisher::ScreenBackend,
};
use nanocodex_hand::{
    recording::{FilteredFrame, FrameMime, MouseButton, SafeEvent, Scope, Store},
    recording_observer::{NativeObserver, ObservedEvent},
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

struct State {
    store: Store,
    // Covers a complete sample/append and every control mutation. Pause/stop
    // cannot acknowledge while a preceding sample can still be persisted.
    gate: Mutex<()>,
    stop: AtomicBool,
    capture_epoch: AtomicU64,
    capture: Mutex<Value>,
    desktop_runtime: Option<PathBuf>,
}
pub(crate) struct Recorder {
    state: Arc<State>,
    control: Option<ControlGuard>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Recorder {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::Release);
        self.control.take();
        // Native calls are bounded by their adapter; joining prevents callbacks
        // from using a replaced desktop or retaining evidence after teardown.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        // Drain a control mutation which passed the stop check before teardown.
        // Otherwise a late start could leave an active manifest without a worker.
        let _gate = self.state.gate.lock();
        let active = self.state.store.active_recording().ok().flatten().is_some();
        let _ = self.state.store.interrupt_active("hand_shutdown");
        if active {
            eprintln!("Hand recording interrupted by Hand shutdown.");
        }
    }
}
impl Recorder {
    pub(crate) async fn start(
        root: &Path,
        desktop_runtime: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        // Validate ownership and every path component before Store::open can
        // create files or tighten permissions on an existing directory.
        let root = hand_recording_control::validate_root(root, true)?;
        let data = hand_recording_control::validate_root(&root.join("data"), true)?;
        let state = Arc::new(State {
            store: Store::open(data).map_err(|e| anyhow::anyhow!(e.to_string()))?,
            gate: Mutex::new(()),
            stop: AtomicBool::new(false),
            capture_epoch: AtomicU64::new(0),
            capture: Mutex::new(json!({"state":"idle"})),
            desktop_runtime,
        });
        let handler = handler(state.clone());
        let worker_state = state.clone();
        let worker = std::thread::Builder::new()
            .name("hand-recording".into())
            .spawn(move || {
                let _exit = WorkerExit(worker_state.clone());
                sample_loop(worker_state);
            })?;
        // Own the worker before publishing controls. A failed or cancelled IPC
        // bind drops this guard; no orphan worker or acknowledged start survives.
        let mut recorder = Self {
            state,
            control: None,
            worker: Some(worker),
        };
        recorder.control = Some(hand_recording_control::start(&root, handler).await?);
        Ok(recorder)
    }
    pub(crate) fn wrap(&self, backend: ScreenBackend) -> ScreenBackend {
        let state = self.state.clone();
        Arc::new(move |input| {
            let state = state.clone();
            let backend = backend.clone();
            Box::pin(async move {
                if input["action"] == "recording" {
                    return Ok(dispatch(state, input).await);
                }
                let capabilities = input["action"] == "capabilities";
                let mut result = backend(input).await?;
                if capabilities {
                    result["recording"] = json!(!state.stop.load(Ordering::Acquire));
                    result["recordingCapabilities"] = capability(&state);
                }
                Ok(result)
            })
        })
    }
    pub(crate) async fn shutdown(self) {
        // Native cleanup can wait for an in-flight sample; keep it off Tokio's reactor.
        let _ = tokio::task::spawn_blocking(move || drop(self)).await;
    }
}
/// A broken, locked or unavailable recorder must never take down screen access.
pub(crate) async fn attach(
    root: Option<&Path>,
    desktop_runtime: Option<PathBuf>,
    backend: ScreenBackend,
) -> (Option<Recorder>, ScreenBackend) {
    let recorder = match root {
        Some(root) => Recorder::start(root, desktop_runtime).await.ok(),
        None => None,
    };
    if let Some(recorder) = recorder {
        let backend = recorder.wrap(backend);
        (Some(recorder), backend)
    } else {
        eprintln!("Hand recording unavailable; screen access remains available.");
        let wrapped: ScreenBackend = Arc::new(move |input| {
            let backend = backend.clone();
            Box::pin(async move {
                let unavailable =
                    json!({"schemaVersion":1,"available":false,"error":"recorder_unavailable"});
                if input["action"] == "recording" {
                    return Ok(
                        json!({"status":"unavailable","error":"recorder_unavailable","capability":unavailable}),
                    );
                }
                let capabilities = input["action"] == "capabilities";
                let mut result = backend(input).await?;
                if capabilities {
                    result["recording"] = json!(false);
                    result["recordingCapabilities"] = unavailable;
                }
                Ok(result)
            })
        });
        (None, wrapped)
    }
}

fn handler(state: Arc<State>) -> Handler {
    Arc::new(move |input| {
        let state = state.clone();
        Box::pin(dispatch(state, input))
    })
}
fn capability(state: &State) -> Value {
    json!({"schemaVersion":1,"available":!state.stop.load(Ordering::Acquire),"operations":["sources","start","pause","resume","stop","status","list","read","frame","export","delete"],"capture":{
        "platform":std::env::consts::OS,
        "native_observer":observer_supported(state),
        "keyboard_content":false,"clipboard":false,"window_titles":false,
        "secure_input_policy":"suppress_unknown_or_sensitive",
        "background":true,"sample_interval_ms":100,
        "local_controls":"nanocodex2 hand-recording --state-dir <recordings> '<JSON request>'"
    }})
}
fn observer_supported(state: &State) -> bool {
    if cfg!(target_os = "linux") {
        state.desktop_runtime.is_some()
    } else {
        cfg!(any(target_os = "macos", target_os = "windows"))
    }
}

async fn dispatch(state: Arc<State>, mut input: Value) -> Value {
    match tokio::task::spawn_blocking(move || {
        let Ok(_gate)=state.gate.lock() else {return json!({"status":"unavailable","error":"recorder_unavailable"})};
        if state.stop.load(Ordering::Acquire) && matches!(input["operation"].as_str(),Some("start"|"resume"|"sources")) {return json!({"status":"unavailable","error":"recorder_stopped"});}
        if let Some(object)=input.as_object_mut() {object.remove("action");}
        if input["operation"] == "sources" {
            if input.as_object().is_none_or(|o|o.len()!=1) {return json!({"status":"invalid","error":"invalid_request"});}
            return match NativeObserver::for_desktop(state.desktop_runtime.as_deref()).and_then(|mut o|o.sample()) {
                Ok(observation)=>json!({"status":"ok","sources":[observation.foreground],"input_capabilities":observation.capabilities,"capability":capability(&state)}),
                Err(error)=>json!({"status":"unavailable","error":error.code,"capability":capability(&state)}),
            };
        }
        if matches!(input["operation"].as_str(),Some("start"|"resume")) && !observer_supported(&state) {
            return json!({"status":"unavailable","error":"native_observer_unavailable","capability":capability(&state)});
        }
        let operation = input["operation"].as_str().unwrap_or("").to_owned();
        match state.store.request(input) {
            Ok(mut response)=>{
                // A pause/resume pair may finish between sampler ticks. Invalidate
                // native input queues even when the sampler never sees paused state.
                if matches!(operation.as_str(), "pause" | "resume") {
                    state.capture_epoch.fetch_add(1, Ordering::AcqRel);
                }
                if matches!(operation.as_str(), "start" | "pause" | "resume" | "stop") {
                    match response["state"].as_str() {
                        Some("recording") => {
                            capture_status(&state,json!({"state":"starting","recording_id":response["id"]}));
                            if operation == "resume" { eprintln!("Hand recording resumed."); }
                            else { eprintln!("Hand recording started."); }
                        },
                        Some("paused") => {
                            capture_status(&state,json!({"state":"paused","recording_id":response["id"]}));
                            eprintln!("Hand recording paused.");
                        },
                        Some("stopped") => {
                            capture_status(&state,json!({"state":"stopped","recording_id":response["id"]}));
                            eprintln!("Hand recording stopped.");
                        },
                        _ => (),
                    }
                }
                response["status"]=json!("ok");
                if matches!(operation.as_str(), "start" | "pause" | "resume" | "stop" | "status") {
                    response["capture"]=capture_for_response(&state,&response);
                }
                response["capability"]=capability(&state);
                response
            },
            Err(error)=>{
                // Store errors are bounded fixed validation messages. Never
                // forward filesystem/native errors or captured content.
                let message=error.to_string();
                let code=if message.contains("not found") {"not_found"} else if message.contains("already") {"conflict"} else {"invalid_request"};
                json!({"status":if code=="conflict"{"busy"}else{"invalid"},"error":code})
            }
        }
    }).await {Ok(value)=>value,Err(_)=>json!({"status":"unavailable","error":"recorder_unavailable"})}
}
fn approved(scope: &Scope, app: &str, window: &str) -> bool {
    !scope.exclude_apps.iter().any(|v| v == app)
        && (scope.apps.iter().any(|v| v == app) || scope.windows.iter().any(|v| v == window))
}
fn capture_for_response(state: &State, response: &Value) -> Value {
    let Ok(capture) = state.capture.lock() else {
        return json!({"state":"error"});
    };
    match response["id"].as_str() {
        Some(id) if capture["recording_id"].as_str() == Some(id) => capture.clone(),
        _ if response["state"] == "idle" => json!({"state":"idle"}),
        _ => json!({"state":"inactive"}),
    }
}
fn capture_status(state: &State, mut status: Value) {
    // Every cached observation belongs to exactly one recording. Never attach a
    // live foreground/context to a historical recording, list or delete result.
    if status.get("recording_id").is_none()
        && status["state"] != "idle"
        && let Ok(Some(id)) = state.store.active_recording()
    {
        status["recording_id"] = json!(id);
    }
    if let Ok(mut current) = state.capture.lock() {
        if current["state"] != status["state"] {
            match status["state"].as_str() {
                Some("recording") => eprintln!("Hand recording capture active."),
                Some("suppressed") => eprintln!("Hand recording capture suppressed."),
                Some("error") => eprintln!("Hand recording capture unavailable."),
                Some("stopped") => eprintln!("Hand recording stopped."),
                _ => (),
            }
        }
        *current = status;
    }
}
// Covers normal errors and unwinding: an exited worker can never accept a new
// start/resume. Read/list/delete controls remain usable for retained evidence.
struct WorkerExit(Arc<State>);
impl Drop for WorkerExit {
    fn drop(&mut self) {
        if !self.0.stop.swap(true, Ordering::AcqRel) {
            let _gate = self.0.gate.lock();
            let _ = self.0.store.interrupt_active("capture_error");
            capture_status(
                &self.0,
                json!({"state":"error","reason":"worker_unavailable"}),
            );
        }
    }
}
fn retain_sample(state: &State, id: &str, result: Result<Value, nanocodex_hand::Error>) -> bool {
    let metadata = match result {
        Ok(metadata) => Some(metadata),
        // Duration expiration can make append return an error after committing
        // the stopped state. Recover that terminal result before failing closed.
        Err(_) => state
            .store
            .request(json!({"operation":"status","id":id}))
            .ok()
            .filter(|metadata| metadata["state"] != "recording"),
    };
    if let Some(metadata) = metadata {
        if metadata["state"] == "recording" {
            return true;
        }
        capture_status(
            state,
            json!({"state":metadata["state"],"recording_id":id,"stop_reason":metadata["stop_reason"]}),
        );
        return false;
    }
    state.stop.store(true, Ordering::Release);
    let _ = state.store.interrupt_active("capture_error");
    capture_status(
        state,
        json!({"state":"error","recording_id":id,"reason":"storage_unavailable"}),
    );
    false
}
fn sample_loop(state: Arc<State>) {
    let mut observer = None;
    let mut previous_id = String::new();
    let mut previous_epoch = 0;
    let mut previous_focus = None;
    let mut previous_suppression = String::new();
    let mut last_pointer = None;
    let mut last_frame = Instant::now();
    'sampling: while !state.stop.load(Ordering::Acquire) {
        let started = Instant::now();
        {
            let Ok(_gate) = state.gate.lock() else { break };
            if state.stop.load(Ordering::Acquire) {
                break;
            }
            match state.store.active_config() {
                Ok(Some((id, scope))) => {
                    let epoch = state.capture_epoch.load(Ordering::Acquire);
                    if id != previous_id || epoch != previous_epoch {
                        observer = None;
                        previous_id = id.clone();
                        previous_epoch = epoch;
                        previous_focus = None;
                        previous_suppression.clear();
                        last_pointer = None;
                    }
                    if observer.is_none() {
                        observer =
                            NativeObserver::for_desktop(state.desktop_runtime.as_deref()).ok();
                    }
                    let result = observer.as_mut().map(|o| o.sample());
                    match result {
                        Some(Ok(observation)) => {
                            let context = &observation.foreground;
                            if !approved(&scope, &context.app_id, &context.window_id) {
                                if previous_suppression != "scope_excluded"
                                    && !retain_sample(
                                        &state,
                                        &id,
                                        state.store.append_event(
                                            &id,
                                            SafeEvent::Suppressed {
                                                reason: "scope_excluded".into(),
                                            },
                                        ),
                                    )
                                {
                                    continue 'sampling;
                                }
                                previous_suppression = "scope_excluded".into();
                                previous_focus = None;
                                last_pointer = None;
                                capture_status(
                                    &state,
                                    json!({"state":"suppressed","reason":"scope_excluded"}),
                                );
                            } else {
                                if previous_focus.as_ref() != Some(context) {
                                    if !retain_sample(
                                        &state,
                                        &id,
                                        state.store.append_event(
                                            &id,
                                            SafeEvent::Focus {
                                                app_id: context.app_id.clone(),
                                                window_id: context.window_id.clone(),
                                            },
                                        ),
                                    ) {
                                        continue 'sampling;
                                    }
                                    previous_focus = Some(context.clone());
                                }
                                let pointer = (observation.pointer.x, observation.pointer.y);
                                // Retain pointer location at meaningful transitions rather
                                // than persisting a continuous mouse trajectory.
                                if last_pointer != Some(pointer) && !observation.events.is_empty() {
                                    if !retain_sample(
                                        &state,
                                        &id,
                                        state.store.append_event(
                                            &id,
                                            SafeEvent::Pointer {
                                                x: pointer.0,
                                                y: pointer.1,
                                            },
                                        ),
                                    ) {
                                        continue 'sampling;
                                    }
                                    last_pointer = Some(pointer);
                                }
                                for event in observation.events {
                                    let event = match event {
                                        ObservedEvent::FocusChanged => continue,
                                        ObservedEvent::Button { button, pressed } => {
                                            SafeEvent::Button {
                                                button: match button {
                                                    1 => MouseButton::Left,
                                                    2 => MouseButton::Middle,
                                                    3 => MouseButton::Right,
                                                    _ => MouseButton::Other,
                                                },
                                                pressed,
                                            }
                                        }
                                        ObservedEvent::Scroll {
                                            horizontal,
                                            vertical,
                                        } => SafeEvent::Scroll {
                                            dx: horizontal,
                                            dy: vertical,
                                        },
                                    };
                                    if !retain_sample(
                                        &state,
                                        &id,
                                        state.store.append_event(&id, event),
                                    ) {
                                        continue 'sampling;
                                    }
                                }
                                let mut frame_status = if scope.capture_frames {
                                    "pending"
                                } else {
                                    "disabled"
                                };
                                if scope.capture_frames
                                    && last_frame.elapsed() >= Duration::from_millis(500)
                                {
                                    last_frame = Instant::now();
                                    let result = observer
                                        .as_mut()
                                        .expect("observer exists after sample")
                                        .frame(context);
                                    match result {
                                        Ok(frame) => {
                                            let retained = state.store.append_frame(
                                                &id,
                                                FilteredFrame {
                                                    bytes: &frame.bytes,
                                                    width: frame.width,
                                                    height: frame.height,
                                                    mime: FrameMime::Jpeg,
                                                },
                                            );
                                            if !retain_sample(&state, &id, retained) {
                                                continue 'sampling;
                                            }
                                            // A recording result also covers consecutive-frame dedup.
                                            frame_status = "retained";
                                            previous_suppression.clear();
                                        }
                                        Err(error) => {
                                            frame_status = match error.code {
                                                "capture_unsupported" => "capture_unsupported",
                                                "capture_suppressed" => "capture_suppressed",
                                                _ => "capture_failed",
                                            };
                                            if previous_suppression != frame_status {
                                                if !retain_sample(
                                                    &state,
                                                    &id,
                                                    state.store.append_event(
                                                        &id,
                                                        SafeEvent::Suppressed {
                                                            reason: frame_status.into(),
                                                        },
                                                    ),
                                                ) {
                                                    continue 'sampling;
                                                }
                                                previous_suppression = frame_status.into();
                                            }
                                        }
                                    }
                                }
                                capture_status(
                                    &state,
                                    json!({"state":"recording","foreground":context,"input_capabilities":observation.capabilities,"frames":frame_status,"event_source":"observed","attribution":"native input may include human or injected actions"}),
                                );
                            }
                        }
                        _ => {
                            observer = None;
                            previous_focus = None;
                            last_pointer = None;
                            if previous_suppression != "native_observation_unavailable" {
                                if !retain_sample(
                                    &state,
                                    &id,
                                    state.store.append_event(
                                        &id,
                                        SafeEvent::Suppressed {
                                            reason: "native_observation_unavailable".into(),
                                        },
                                    ),
                                ) {
                                    continue 'sampling;
                                }
                                previous_suppression = "native_observation_unavailable".into();
                            }
                            capture_status(
                                &state,
                                json!({"state":"suppressed","reason":"native_observation_unavailable"}),
                            );
                        }
                    }
                }
                Ok(None) => {
                    observer = None;
                    previous_id.clear();
                    previous_focus = None;
                    last_pointer = None;
                    capture_status(&state, json!({"state":"idle"}));
                }
                Err(_) => {
                    capture_status(
                        &state,
                        json!({"state":"error","reason":"storage_unavailable"}),
                    );
                    break;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100).saturating_sub(started.elapsed()));
    }
}
pub(crate) async fn serve(root: PathBuf, desktop_runtime: Option<PathBuf>) -> anyhow::Result<()> {
    let recorder = Recorder::start(&root, desktop_runtime).await?;
    eprintln!("Hand recording controls ready. Capture starts only on an explicit start request.");
    super::service::shutdown_signal().await?;
    recorder.shutdown().await;
    Ok(())
}
