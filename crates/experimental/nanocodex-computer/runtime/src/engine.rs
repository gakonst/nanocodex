use crate::{
    Error, Result,
    ax::{Node, Sessions},
    browser::Browsers,
    native::{Action, App, Desktop, Target},
    protocol::IPC_VERSION,
    selection::{self, Mode},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

pub struct Engine {
    pub runtime_backend: crate::runtime::RuntimeBackend,
    pub desktop: Box<dyn Desktop>,
    pub owner: String,
    pub preview: Option<crate::preview::Preview>,
    pub guardian: crate::guardian::Guardian,
    pub guardian_monitor: Option<crate::process_rpc::Program>,
    active_lease: Option<String>,
    pub sessions: Sessions,
    pub browsers: Browsers,
    pub host_turns: Option<crate::host_turns::Controller>,
    apps: BTreeMap<String, App>,
    pub security: crate::security::Security,
    pub auth: crate::auth::Auth,
    pub services: Option<crate::host_services::Services>,
    pub messaging: crate::messaging::Messaging,
    pub platforms: crate::platforms::Platforms,
    media: crate::media::MediaStore,
    approvals: crate::approvals::Approvals,
    kernel_scope: String,
    pub(crate) navigation_security: crate::browser_navigation_security::Owner,
    recording: Option<KernelRecording>,
}
struct KernelRecording {
    scope: String,
    configured: bool,
}
impl Engine {
    pub fn new(desktop: Box<dyn Desktop>) -> Self {
        Self {
            runtime_backend: Default::default(),
            desktop,
            owner: format!("skyre-{}", std::process::id()),
            preview: None,
            guardian: Default::default(),
            guardian_monitor: None,
            active_lease: None,
            sessions: Sessions::default(),
            browsers: Browsers::default(),
            host_turns: None,
            apps: BTreeMap::new(),
            security: Default::default(),
            auth: Default::default(),
            services: None,
            platforms: Default::default(),
            messaging: Default::default(),
            media: Default::default(),
            approvals: Default::default(),
            kernel_scope: "initial".into(),
            navigation_security: Default::default(),
            recording: None,
        }
    }
    /// Trusted host route selection; never available as a model RPC.
    pub fn select_kernel_scope(&mut self, route: &crate::host_turns::Route) {
        let next_scope = route.key();
        if next_scope != self.kernel_scope {
            self.browsers
                .cancel_raw_wait_scope(&self.kernel_scope, None);
            self.navigation_security.reset(&self.kernel_scope);
        }
        self.kernel_scope = next_scope;
        self.security
            .set_download_scope(route.thread_id.as_deref().unwrap_or(&route.conversation_id));
    }
    pub fn selected_kernel_scope(&self) -> &str {
        &self.kernel_scope
    }
    pub fn begin_chooser_cell(&mut self, cell: u64) -> String {
        let scope = self.kernel_scope.clone();
        self.browsers.begin_chooser_cell(&scope, cell);
        self.navigation_security.begin(&scope, cell);
        scope
    }
    pub fn finish_chooser_cell(&mut self, scope: &str, cell: u64, cancelled: bool) {
        self.browsers.finish_chooser_cell(scope, cell, cancelled);
        self.navigation_security.finish(scope, cell);
    }
    /// Reset only resources owned by the selected JavaScript kernel. Parked
    /// route kernels retain their recordings, and external app/browser state lives on.
    pub fn reset_kernel_resources(&mut self) -> Result<()> {
        self.reset_kernel_scope_resources(&self.kernel_scope.clone())
    }
    /// A parked kernel may fail while another route is selected. Clean its
    /// resources without changing the selected route or touching another owner.
    pub fn reset_kernel_scope_resources(&mut self, scope: &str) -> Result<()> {
        self.browsers.reset_chooser_scope(scope);
        self.navigation_security.reset(scope);
        self.security.clear_origin_scope(scope);
        if let Some(recording) = &self.recording
            && recording.scope == scope
        {
            if recording.configured {
                self.platforms.cancel_sky_audio()?;
            } else {
                self.desktop.cancel_audio(&self.owner)?;
            }
            self.recording = None;
        }
        Ok(())
    }
    fn revoke_native_control(&mut self) {
        // A later renewed/reacquired lease cannot revive an earlier wait.
        self.browsers.cancel_all_raw_waits();
        let configured = self.platforms.cancel_sky_audio();
        let native = self.desktop.end_session(&self.owner);
        if self.recording.as_ref().is_some_and(|recording| {
            if recording.configured {
                configured.is_ok()
            } else {
                native.is_ok()
            }
        }) {
            self.recording = None;
        }
    }
    fn execute_audio(&mut self, method: &str, params: &Value, configured: bool) -> Result<Value> {
        let method = match method {
            "start_audio_recording" => "start",
            "stop_audio_recording" => "stop",
            other => other,
        };
        if let Some(recording) = &self.recording {
            if recording.scope != self.kernel_scope {
                return Err(Error::new(
                    -32001,
                    "Audio recording belongs to another kernel",
                ));
            }
            if method == "start" {
                return Err(Error::action("computer audio recording is already active"));
            }
            if recording.configured != configured {
                return Err(Error::action("Audio provider changed during recording"));
            }
        }
        let result = if configured {
            let method = match method {
                "start" => "start_audio_recording",
                "stop" => "stop_audio_recording",
                _ => return Err(Error::unsupported("Unsupported configured audio method")),
            };
            self.platforms.sky_audio(method, params)
        } else {
            self.desktop.audio(method, &self.owner, params)
        };
        if result.is_ok() {
            if method == "start" {
                self.recording = Some(KernelRecording {
                    scope: self.kernel_scope.clone(),
                    configured,
                });
            } else if method == "stop" {
                self.recording = None;
            }
        }
        // A failed stop may still own a live provider. Keep its owner until
        // explicit reset or session cleanup confirms cancellation.
        result
    }
    /// Opt-in host control channel. Intentionally absent from execute and JS RPC.
    pub fn authorize_host_turn_request(&self, args: &Value) -> Result<()> {
        self.host_turns
            .as_ref()
            .ok_or_else(|| Error::new(-32003, "Trusted host lifecycle channel is not configured"))?
            .authorize(args["authorityToken"].as_str().unwrap_or(""))
    }
    pub fn host_turn_event(&mut self, args: &Value) -> Result<Value> {
        let controller = self.host_turns.as_mut().ok_or_else(|| {
            Error::new(-32003, "Trusted host lifecycle channel is not configured")
        })?;
        let token = args["authorityToken"]
            .as_str()
            .ok_or_else(|| Error::new(-32003, "Trusted host turn capability is required"))?;
        let event = serde_json::from_value(args["event"].clone())?;
        controller.event(token, event, &mut self.browsers)
    }
    pub fn host_recovery(&mut self, args: &Value, resolve: bool) -> Result<Value> {
        let controller = self.host_turns.as_ref().ok_or_else(|| {
            Error::new(-32003, "Trusted host lifecycle channel is not configured")
        })?;
        controller.recovery(
            args["authorityToken"].as_str().unwrap_or(""),
            args,
            &mut self.browsers,
            resolve,
        )
    }
    fn app(&mut self, identifier: &str) -> Result<App> {
        if self.security.check_app(identifier).is_err() {
            let candidate = self
                .desktop
                .apps()?
                .into_iter()
                .find(|a| a.id == identifier || a.path == identifier || a.name == identifier);
            if !candidate.as_ref().is_some_and(|a| {
                [a.id.as_str(), a.path.as_str(), a.name.as_str()]
                    .iter()
                    .any(|s| self.security.check_app(s).is_ok())
            }) {
                self.security.check_app(identifier)?;
            }
        }
        if let Some(app) = self.apps.get(identifier).cloned() {
            if self
                .desktop
                .apps()?
                .iter()
                .any(|a| a.pid == app.pid && a.path == app.path && a.id == app.id)
            {
                return Ok(app);
            }
            self.apps.retain(|_, a| a.path != app.path);
            self.sessions.revisions.remove(&app.path);
            return Err(Error::action(
                "Application session ended; bind it again before acting",
            ));
        }
        let app = self.desktop.bind(identifier)?;
        self.apps.insert(identifier.into(), app.clone());
        self.apps.insert(app.path.clone(), app.clone());
        Ok(app)
    }
    fn node(&mut self, app: &App, args: &Value) -> Result<Node> {
        let id = index(args)?;
        let root = self.desktop.snapshot(app)?;
        self.sessions.resolve(&app.path, id, root, false)
    }
    fn target(&mut self, app: &App, args: &Value) -> Result<Target> {
        if args
            .get("element_index")
            .or_else(|| args.get("elementIndex"))
            .is_some()
        {
            Ok(Target::Element {
                identity: self.node(app, args)?.identity,
            })
        } else {
            Ok(Target::Point {
                point: point(args, "point", "x", "y")?,
            })
        }
    }
    fn refresh_guardian(&mut self) -> Result<()> {
        let Some(monitor) = &self.guardian_monitor else {
            return Ok(());
        };
        let report = monitor.request(&json!({"type":"guardian_state","owner":self.owner}))?;
        let revision = report["revision"]
            .as_u64()
            .ok_or_else(|| Error::action("Guardian monitor missing revision"))?;
        let locked = report["locked"]
            .as_bool()
            .ok_or_else(|| Error::action("Guardian monitor missing lock state"))?;
        let current = self.guardian.status()["host_revision"].as_u64();
        match report["event"].as_str().unwrap_or("snapshot") {
            "snapshot" => self.guardian.set_host_state(locked, revision),
            "intervention" if current == Some(revision) => Ok(()),
            "intervention" => self.guardian.intervene(revision),
            "resume" if current == Some(revision) => Ok(()),
            "resume" => self.guardian.resume_after_intervention(revision),
            _ => Err(Error::action("Unknown guardian monitor event")),
        }
    }
    pub fn tick(&mut self) {
        self.browsers.tick_choosers();
        if self.guardian_monitor.is_some()
            && let Some(lease) = self.active_lease.clone()
            && self
                .refresh_guardian()
                .and_then(|_| self.guardian.authorize(&self.owner, &lease))
                .is_err()
        {
            self.active_lease = None;
            self.preview = None;
            self.revoke_native_control();
        }
        // Pure native timer delivery only, after guardian refresh/revocation.
        // Socket receipt remains in a validated bounded continuation call.
        self.browsers.tick_raw_wait_timers(
            &self.kernel_scope,
            &self.security,
            self.guardian_monitor
                .as_ref()
                .and(self.active_lease.as_deref()),
        );
        if self.preview.as_ref().is_some_and(|p| p.closed()) {
            self.preview = None;
            return;
        }
        if let Some(app) = self
            .preview
            .as_ref()
            .filter(|p| p.due())
            .map(|p| p.app.clone())
        {
            let result = self.app(&app).and_then(|app| self.desktop.screenshot(&app));
            match result {
                Ok(image) => {
                    if self
                        .preview
                        .as_mut()
                        .unwrap()
                        .publish(&self.owner, image)
                        .is_err()
                    {
                        self.preview = None;
                    }
                }
                Err(_) => self.preview = None,
            }
        }
    }
    pub fn end_session(&mut self) {
        self.browsers.cancel_all_raw_waits();
        self.approvals.clear();
        self.security.clear_download_approvals();
        self.security.clear_origin_grants();
        self.navigation_security.clear();
        self.preview = None;
        let _ = self.guardian.revoke_owner(&self.owner);
        self.active_lease = None;
        self.auth.shutdown(&self.security);
        if let Some(services) = &mut self.services {
            let _ = services.end_session();
        }
        for (browser, error) in self.browsers.end_session() {
            eprintln!("Browser {browser} cleanup failed: {}", error.message);
        }
        let _ = self.platforms.cancel_sky_audio();
        let _ = self.platforms.end_turn();
        let _ = self.desktop.end_session(&self.owner);
        self.recording = None;
        self.kernel_scope = "initial".into();
        self.security.set_download_scope("");
    }
    fn execute_browser_with_control(
        &mut self,
        method: &str,
        args: &Value,
        admitted: bool,
        control: Option<&crate::runtime::ProviderControl>,
    ) -> Result<Value> {
        self.execute_browser_with_admissions(method, args, admitted, control, None)
    }
    pub(crate) fn execute_browser_with_model(
        &mut self,
        method: &str,
        args: &Value,
        admitted: bool,
        model: Option<&crate::browser_activation::Model>,
    ) -> Result<Value> {
        // Stored metadata is refusal-only data. It supplies neither execution
        // validity nor before-unload/approval authority to a later dispatch.
        self.execute_browser_with_admissions(method, args, admitted, None, model)
    }
    fn execute_browser_with_admissions(
        &mut self,
        method: &str,
        args: &Value,
        admitted: bool,
        control: Option<&crate::runtime::ProviderControl>,
        captured_model: Option<&crate::browser_activation::Model>,
    ) -> Result<Value> {
        self.browsers.set_download_security(self.security.clone());
        if method == "webmcp_invoke_tool" {
            // Keep the existing restricted-command refusal ahead of canonical
            // argument parsing. This reads no fetched state or provider data.
            self.security.check_browser_command("webmcp_invoke")?;
        }
        // Policy and dispatch must see the same canonical operation,
        // including selector-encoded frames and nested AX action aliases.
        let (method, mut args) = Browsers::normalize_request(method, args)?;
        let method = method.as_str();
        self.security.check_browser_command(method)?;
        self.security.check_browser_frame(method, &args)?;
        if method == "cdp_events" && args.get("__skyreRawWait").is_some() {
            // Selection by JSON is not admission. This native entry point must
            // validate stored policy, cell, provider and attachment provenance
            // before polling/consuming, and retire a matched revoked operation.
            // It performs neither document_context nor fresh origin admission.
            let expected_lease = self.active_lease.clone();
            let result = self.browsers.continue_raw_wait(
                &args,
                &self.kernel_scope,
                &self.security,
                self.guardian_monitor
                    .as_ref()
                    .and(expected_lease.as_deref()),
            );
            if result.is_ok() && self.guardian_monitor.is_some() {
                // Receipt callbacks can consume time or observe a changed host.
                // Hold every successful packet until the owning Engine checks
                // the live lease again. A consumed terminal remains at-most-once
                // when this final admission rejects it; no held data is exposed.
                let checked = (|| -> Result<()> {
                    self.refresh_guardian()?;
                    let lease = expected_lease
                        .as_deref()
                        .ok_or_else(|| Error::new(-32003, "Raw event wait control lease ended"))?;
                    if self.active_lease != expected_lease {
                        return Err(Error::new(-32003, "Raw event wait control lease changed"));
                    }
                    self.guardian.authorize(&self.owner, lease)
                })();
                if let Err(error) = checked {
                    self.browsers.cancel_all_raw_waits();
                    return Err(error);
                }
            }
            return result;
        }
        // Registration admission reads only native state after command/frame
        // policy. It cannot discover or attach a provider or trust model metadata.
        self.browsers.prepare_webmcp(method, &mut args)?;
        if self.security.origin_approval_required()
            && !admitted
            && self.navigation_security.waiting_on(&args)
            && method != "close_tab"
        {
            return Err(Error::new(
                -32014,
                "Browser navigation authorization is pending",
            ));
        }
        if method == "close_tab" {
            self.navigation_security.close(&args);
        }
        if (method == "navigate" || method == "new_tab")
            && let Some(url) = args["url"].as_str()
        {
            self.security.check_url(url)?;
        }
        if self.security.browser_restricted()
            && args["tab"].is_string()
            && !(self.security.origin_approval_required()
                && crate::browser_navigation_security::exempt(method, &args))
            && !["navigate", "new_tab"].contains(&method)
        {
            let main_args = json!({"browser":args["browser"],"tab":args["tab"]});
            let main = self.browsers.execute("document_context", &main_args)?;
            self.security.check_browser_document(&main)?;
            let context = if args.get("frame").is_some() {
                let target = self.browsers.execute("document_context", &args)?;
                self.security.check_browser_document(&target)?;
                target
            } else {
                main
            };
            if crate::security::Security::browser_document_bound(method) {
                for (expected, observed) in [
                    ("expectedDocumentToken", "documentToken"),
                    ("expectedUrl", "url"),
                ] {
                    if args.get(expected).is_some_and(|v| v != &context[observed]) {
                        return Err(Error::new(
                            -32014,
                            "Document changed before policy-bound action",
                        ));
                    }
                    args[expected] = context[observed].clone();
                }
                // Never resolve a CSS frame selector again after checking
                // its origin: a replacement frame must fail the binding.
                args["frame"] = context["frameId"].clone();
            }
        }
        if self.security.origin_approval_required() {
            self.check_direct_origin_access(method, &mut args, admitted)?;
        }
        // This optional native capability is derived only after the existing
        // normalized command/frame/URL/document/origin checks above.
        let automatic =
            control.and_then(|control| self.beforeunload_runtime_admission(method, &args, control));
        let mut result = if method == "cdp_events" {
            // Reached only after the ordinary command/frame/origin checks.
            // None permits the existing immediate read only. A result that
            // would wait must fail closed without immutable native provenance.
            self.browsers.execute_raw_events_admitted(
                &args,
                &self.kernel_scope,
                self.security.raw_wait_policy(),
                self.guardian_monitor
                    .as_ref()
                    .and(self.active_lease.as_deref()),
            )?
        } else if crate::browser_activation::command(method).is_some() {
            let model = if let Some(model) = captured_model {
                Some(model)
            } else {
                control
                    .map(|control| control.activation_model())
                    .transpose()?
            };
            self.browsers.execute_webmcp_admitted(
                method,
                &args,
                model.unwrap_or(&Default::default()),
            )?
        } else if let Some(automatic) = automatic.as_ref() {
            self.browsers
                .execute_beforeunload_admitted(method, &args, automatic)?
        } else {
            self.browsers.execute_normalized(method, &args)?
        };
        if self.security.origin_approval_required()
            && [
                "info",
                "get_browser",
                "get_default_browser",
                "get_browser_for_url",
            ]
            .contains(&method)
        {
            result["_skyreOriginApproval"] = json!(true);
        }
        Ok(result)
    }
    /// Agent JavaScript can use the CUA facade and explicitly exposed extensions.
    /// Legacy native RPC remains available to trusted local CLI/socket callers,
    /// but must not bypass the facade's host-owned approval grants from a cell.
    pub fn execute_from_js(&mut self, method: &str, args: &Value) -> Result<Value> {
        self.execute_from_js_with_control(method, args, None)
    }
    pub(crate) fn execute_from_js_with_control(
        &mut self,
        method: &str,
        args: &Value,
        control: Option<&crate::runtime::ProviderControl>,
    ) -> Result<Value> {
        if method.starts_with("host/") || method.starts_with("host.turn") {
            return Err(Error::new(
                -32003,
                "Trusted host lifecycle is not a model API",
            ));
        }
        let native = method.strip_prefix("sky.").unwrap_or(method);
        if [
            "bind_app",
            "get_app_state",
            "get_screenshot",
            "click",
            "drag",
            "press_key",
            "type_text",
            "paste",
            "set_value",
            "select_text",
            "scroll",
            "perform_secondary_action",
        ]
        .contains(&native)
            || method.starts_with("audio.")
            || method.starts_with("preview.")
            || method.starts_with("platform.")
        {
            return Err(Error::new(
                -32003,
                "Use the approved cua.computer or cua.getApp interface for native operations",
            ));
        }
        self.execute_with_control(method, args, control)
    }
    pub fn execute(&mut self, method: &str, args: &Value) -> Result<Value> {
        self.execute_with_control(method, args, None)
    }
    fn execute_with_control(
        &mut self,
        method: &str,
        args: &Value,
        control: Option<&crate::runtime::ProviderControl>,
    ) -> Result<Value> {
        let started = std::time::Instant::now();
        let mut args = args.clone();
        let lease = args.as_object_mut().and_then(|o| o.remove("guardianLease"));
        let protected = (method.starts_with("sky.") && method != "sky.setup")
            || method.starts_with("browser.")
            || method.starts_with("auth.")
            || method.starts_with("preview.")
            || method.starts_with("audio.")
            || method == "platform.call"
            || [
                "bind_app",
                "get_app_state",
                "get_screenshot",
                "click",
                "drag",
                "press_key",
                "type_text",
                "paste",
                "set_value",
                "select_text",
                "scroll",
                "perform_secondary_action",
            ]
            .contains(&method);
        let result = (|| {
            if protected && self.guardian_monitor.is_some() {
                let authorization = (|| -> Result<()> {
                    self.refresh_guardian()?;
                    let token = lease
                        .as_ref()
                        .and_then(Value::as_str)
                        .ok_or_else(|| Error::new(-32003, "A control lease is required"))?;
                    self.guardian.authorize(&self.owner, token)?;
                    if self.active_lease.as_deref() != Some(token) {
                        self.browsers.cancel_all_raw_waits();
                    }
                    self.active_lease = Some(token.into());
                    Ok(())
                })();
                if let Err(error) = authorization {
                    // Even a subsequent valid request cannot revive a wait
                    // after observed host lock/intervention/lease loss.
                    self.browsers.cancel_all_raw_waits();
                    return Err(error);
                }
            }
            self.execute_inner_controlled(method, &args, control)
        })();
        if !method.starts_with("host.")
            && let Some(services) = &mut self.services
        {
            let _=services.record("provider_call",json!({"method":method,"success":result.is_ok(),"code":result.as_ref().err().map(|e|e.code),"durationMicros":started.elapsed().as_micros().min(u64::MAX as u128) as u64}));
        }
        result
    }
    // Existing internal native lowerings remain uncontrolled. Only the explicit
    // native provider entrypoint threads a before-unload admission candidate.
    fn execute_inner(&mut self, method: &str, args: &Value) -> Result<Value> {
        self.execute_inner_controlled(method, args, None)
    }
    fn execute_inner_controlled(
        &mut self,
        method: &str,
        args: &Value,
        control: Option<&crate::runtime::ProviderControl>,
    ) -> Result<Value> {
        if method == "sky.app_policy" {
            let app = self.desktop.app_policy_target(string(args, "app")?)?;
            return Ok(self.approvals.policy(app, &self.security));
        }
        if method == "sky.windows_policy" {
            if self.desktop.sky_target() != "windows" {
                return Err(Error::unsupported(
                    "Windows policy requires a Windows desktop",
                ));
            }
            let app = self
                .desktop
                .sky_policy_target(string(args, "method")?, &args["params"])?;
            let required = self.approvals.authorize_app(&app.path).is_err();
            let mut policy = self.approvals.policy(app, &self.security);
            policy["approvalRequired"] = json!(required);
            return Ok(policy);
        }
        if method == "host.elicitation" {
            let (_, response) = self.prepare_elicitation(args)?;
            return response.ok_or_else(|| {
                Error::unsupported("Computer Use requires a host that supports elicitations")
            });
        }
        if method == "sky.setup" {
            return Ok(self.sky_setup());
        }
        if method == "sky.execute" {
            return self.sky_execute(args);
        }
        if let Some(method) = method.strip_prefix("sky.drag_") {
            if self.desktop.sky_target() != "linux" {
                return Err(Error::unsupported("Drag handles require a Linux desktop"));
            }
            return self.desktop.sky_execute(&format!("drag_{method}"), args);
        }
        if let Some(method) = method.strip_prefix("guardian.") {
            self.refresh_guardian()?;
            let token = || string(args, "lease");
            return match method {
                "status" => Ok(self.guardian.status()),
                "acquire" => Ok(serde_json::to_value(
                    self.guardian.acquire(
                        &self.owner,
                        args.get("ttl_ms")
                            .map(|v| {
                                v.as_u64()
                                    .ok_or_else(|| Error::invalid("ttl_ms must be unsigned"))
                            })
                            .transpose()?
                            .unwrap_or(30000),
                    )?,
                )?),
                "renew" => Ok(serde_json::to_value(
                    self.guardian.renew(
                        &self.owner,
                        token()?,
                        args["ttl_ms"]
                            .as_u64()
                            .ok_or_else(|| Error::invalid("ttl_ms must be unsigned"))?,
                    )?,
                )?),
                "release" => {
                    let released = self.guardian.release(&self.owner, token()?)?;
                    self.active_lease = None;
                    self.preview = None;
                    self.revoke_native_control();
                    Ok(json!({"released":released}))
                }
                _ => Err(Error::unsupported("Unknown control lease operation")),
            };
        }
        if let Some(method) = method.strip_prefix("preview.") {
            return match method {
                "start" => {
                    if self.preview.is_some() {
                        return Err(Error::action("A preview is already active"));
                    }
                    let app = self.app(string(args, "app")?)?;
                    let duration = args
                        .get("duration_ms")
                        .map(|v| {
                            v.as_u64()
                                .ok_or_else(|| Error::invalid("duration_ms must be unsigned"))
                        })
                        .transpose()?
                        .unwrap_or(30000);
                    let interval = args
                        .get("interval_ms")
                        .map(|v| {
                            v.as_u64()
                                .ok_or_else(|| Error::invalid("interval_ms must be unsigned"))
                        })
                        .transpose()?
                        .unwrap_or(500);
                    let mut preview = crate::preview::Preview::start(
                        &self.owner,
                        &app.path,
                        std::time::Duration::from_millis(duration),
                        std::time::Duration::from_millis(interval),
                    )?;
                    preview.publish(&self.owner, self.desktop.screenshot(&app)?)?;
                    let result = preview.status(&self.owner)?;
                    self.preview = Some(preview);
                    Ok(result)
                }
                "status" => self
                    .preview
                    .as_ref()
                    .ok_or_else(|| Error::action("No active preview"))?
                    .status(&self.owner),
                "stop" => {
                    if let Some(mut preview) = self.preview.take() {
                        preview.close(&self.owner)?;
                    }
                    Ok(json!({"closed":true}))
                }
                _ => Err(Error::unsupported("Unknown preview method")),
            };
        }
        if method == "security.policy" {
            return Ok(self.security.snapshot());
        }
        if let Some(method) = method.strip_prefix("auth.") {
            return self
                .auth
                .execute(method, args, &mut self.browsers, &self.security);
        }
        if method.starts_with("host.") {
            return self
                .services
                .as_mut()
                .ok_or_else(|| Error::action("Host storage is disabled; configure --data-dir"))?
                .execute(method, args);
        }
        if method == "messages.review" {
            let id = string(args, "plan_id")?;
            let plan = self
                .messaging
                .execute("messages.status", &json!({"plan_id":id}))?;
            self.security
                .review(json!({"operation":"send_message","plan":plan}))?;
            self.messaging
                .authorize(id, string(&plan, "digest")?, "auto_review")?;
            return Ok(json!({"plan_id":id,"digest":plan["digest"],"state":"approved"}));
        }
        if method.starts_with("messages.") {
            return self.messaging.execute(method, args);
        }
        if let Some(method) = method.strip_prefix("audio.") {
            let mut params = args.clone();
            if method == "start" && args["scope"] != "system" {
                let app = self.app(string(args, "app")?)?;
                params["pid"] = json!(app.pid);
            }
            return self.execute_audio(method, &params, false);
        }
        if method.starts_with("platform.") {
            return self.platforms.execute(method, args);
        }
        if let Some(method) = method.strip_prefix("browser.") {
            if method == "origin_operation_start" {
                return self.start_origin_operation(args, control);
            }
            if method == "origin_operation_poll" {
                return self.poll_origin_operation(args);
            }
            return self.execute_browser_with_control(method, args, false, control);
        }
        if let Some(method) = method.strip_prefix("fixture.") {
            return self.desktop.control_fixture(method, args);
        }
        let method = method.strip_prefix("sky.").unwrap_or(method);
        if method == "capabilities" {
            return Ok(
                json!({"desktop":self.desktop.capabilities(),"browser":self.browsers.capabilities(),"runtime":self.runtime_backend,"available_runtimes":crate::runtime::RuntimeBackend::available(),"protocols":["jsonrpc-2.0","mcp-stdio","CodexComputerUseIPC-5"]}),
            );
        }
        if method == "list_apps" {
            return Ok(serde_json::to_value(self.desktop.apps()?)?);
        }
        if ![
            "bind_app",
            "get_app_state",
            "get_screenshot",
            "click",
            "drag",
            "press_key",
            "type_text",
            "paste",
            "set_value",
            "select_text",
            "scroll",
            "perform_secondary_action",
        ]
        .contains(&method)
        {
            return Err(Error::unsupported(format!(
                "Unsupported desktop method: {method}"
            )));
        }
        let identifier = string(args, "app")?;
        let app = self.app(identifier)?;
        if method == "bind_app" {
            return Ok(serde_json::to_value(app)?);
        }
        if method == "get_app_state" {
            // Only a successfully published observation establishes coordinate
            // geometry. Preview captures and failed observations cannot replace it.
            self.desktop.invalidate_screenshot(&app);
            let result = (|| {
                let root = self.desktop.snapshot(&app)?;
                let title = root.title.clone().unwrap_or_else(|| app.name.clone());
                let full = args
                    .get("disableDiff")
                    .or_else(|| args.get("disableDiffing"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let screenshot = if args["screenshot"].as_bool().unwrap_or(false) {
                    Some(self.desktop.screenshot_for_observation(&app)?)
                } else {
                    None
                };
                let (description, revision) = self.sessions.observe(&app.path, root, full)?;
                let state = crate::ax::format_state(&title, &app.name, &description, &revision);
                let instructions = self.desktop.app_specific_instructions(&app);
                let mut result = json!({"app":app.path,"pid":app.pid,"bundleIdentifier":app.id,"name":app.name,"state":state,"tree":revision.root,"focusTree":revision.focus,"focusState":revision.focus_text(),"revision":revision.generation,"screenshot":screenshot,"observationDiagnostics":self.desktop.diagnostics(&app)});
                if let Some(instructions) = instructions {
                    result["appSpecificInstructions"] = json!(instructions);
                }
                Ok(result)
            })();
            if result.is_err() {
                self.desktop.invalidate_screenshot(&app);
            }
            return result;
        }
        if method == "get_screenshot" {
            return Ok(serde_json::to_value(self.desktop.screenshot(&app)?)?);
        }
        let action = match method {
            "click" => {
                let button = match args.get("mouse_button").or_else(|| args.get("mouseButton")) {
                    None => 0,
                    Some(Value::Number(n)) => n
                        .as_u64()
                        .filter(|n| *n <= 2)
                        .ok_or_else(|| Error::invalid("Invalid mouse button"))?
                        as u8,
                    Some(Value::String(s)) => match s.trim().to_lowercase().as_str() {
                        "left" | "l" => 0,
                        "right" | "r" => 1,
                        "middle" | "m" => 2,
                        _ => return Err(Error::invalid("Invalid mouse button")),
                    },
                    _ => return Err(Error::invalid("Invalid mouse button")),
                };
                let count = args
                    .get("click_count")
                    .or_else(|| args.get("clickCount"))
                    .map(|v| {
                        v.as_u64()
                            .ok_or_else(|| Error::invalid("clickCount must be an integer"))
                    })
                    .transpose()?
                    .unwrap_or(1);
                if !(1..=3).contains(&count) {
                    return Err(Error::invalid("clickCount must be 1..3"));
                }
                Action::Click {
                    target: self.target(&app, args)?,
                    button,
                    count: count as u32,
                }
            }
            "drag" => Action::Drag {
                from: point(args, "from", "from_x", "from_y")?,
                to: point(args, "to", "to_x", "to_y")?,
            },
            "press_key" => Action::PressKey {
                key: string(args, "key")?.into(),
            },
            "type_text" => Action::TypeText {
                text: string(args, "text")?.into(),
            },
            "paste" => {
                let format = args["format"].as_str().unwrap_or("text");
                if !["text", "md", "html"].contains(&format) {
                    return Err(Error::invalid("Unknown paste format"));
                }
                Action::Paste {
                    text: string(args, "text")?.into(),
                    format: format.into(),
                }
            }
            "set_value" => {
                let node = self.node(&app, args)?;
                if !node.settable {
                    return Err(Error::action(
                        "Cannot set a value for an element that is not settable",
                    ));
                }
                Action::SetValue {
                    identity: node.identity,
                    value: string(args, "value")?.into(),
                }
            }
            "select_text" => {
                let node = self.node(&app, args)?;
                let mode = args
                    .get("selection")
                    .or_else(|| args.get("selectionType"))
                    .or_else(|| args.get("selection_type"))
                    .cloned()
                    .unwrap_or(json!("text"));
                let mode: Mode = serde_json::from_value(mode)
                    .map_err(|_| Error::invalid("Invalid selection mode"))?;
                let range = selection::select_node(
                    &node,
                    string(args, "text")?,
                    args["prefix"].as_str(),
                    args["suffix"].as_str(),
                    mode,
                )?;
                Action::SelectText {
                    identity: node.identity,
                    range,
                }
            }
            "scroll" => {
                let direction = match string(args, "direction")?.trim().to_lowercase().as_str() {
                    "u" | "up" => "up",
                    "d" | "down" => "down",
                    "l" | "left" => "left",
                    "r" | "right" => "right",
                    _ => return Err(Error::invalid("Invalid scroll direction")),
                };
                let pages = args
                    .get("pages")
                    .map(|v| {
                        v.as_f64()
                            .ok_or_else(|| Error::invalid("pages must be a number"))
                    })
                    .transpose()?
                    .unwrap_or(1.0);
                if !pages.is_finite() || pages <= 0.0 {
                    return Err(Error::invalid("pages must be finite and positive"));
                }
                Action::Scroll {
                    target: self.target(&app, args)?,
                    direction: direction.into(),
                    pages,
                }
            }
            "perform_secondary_action" => {
                let node = self.node(&app, args)?;
                let action = string(args, "action")?;
                let action = node
                    .action_named(action)
                    .ok_or_else(|| Error::invalid("Action was not advertised by this element"))?
                    .to_owned();
                Action::Secondary {
                    identity: node.identity,
                    action,
                }
            }
            _ => {
                return Err(Error::unsupported(format!(
                    "Unsupported desktop method: {method}"
                )));
            }
        };
        let validate_after = matches!(&action, Action::SetValue { .. });
        self.desktop.action(&app, action)?;
        // A setter may have succeeded even if subsequent target validation fails.
        // Preserve that observable ordering; never imply rollback on an error.
        if validate_after {
            let fresh = self.desktop.snapshot(&app)?;
            self.sessions
                .resolve(&app.path, index(args)?, fresh, true)?;
        }
        Ok(Value::Null)
    }
    fn sky_setup(&self) -> Value {
        let target = self.desktop.sky_target();
        let mut methods = match target {
            "mac" => vec![
                "list_apps",
                "get_app_state",
                "click",
                "drag",
                "paste",
                "perform_secondary_action",
                "press_key",
                "scroll",
                "select_text",
                "set_value",
                "type_text",
            ],
            "linux" => vec![
                "click",
                "drag",
                "drag_handle",
                "get_screenshot",
                "move",
                "press_key",
                "scroll",
                "type_text",
            ],
            "windows" => vec![
                "activate_window",
                "click",
                "drag",
                "get_window",
                "get_window_state",
                "launch_app",
                "list_windows",
                "list_apps",
                "perform_secondary_action",
                "press_key",
                "scroll",
                "set_value",
                "type_text",
            ],
            _ => vec![],
        };
        if std::env::var("SKY_ENABLE_AUDIO").as_deref() == Ok("1") {
            methods.extend(["start_audio_recording", "stop_audio_recording"]);
        }
        json!({"target":target,"methods":methods})
    }
    fn sky_execute(&mut self, request: &Value) -> Result<Value> {
        let method = string(request, "method")?;
        let arguments = request["args"]
            .as_array()
            .ok_or_else(|| Error::invalid("Sky arguments must be an array"))?;
        if !self.sky_setup()["methods"]
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == method)
        {
            return Err(Error::unsupported(format!(
                "Sky method unavailable: {method}"
            )));
        }
        let params = arguments.first().cloned().unwrap_or(json!({}));
        if method == "start_audio_recording" {
            if self.desktop.sky_target() == "linux" {
                return self.execute_audio("start", &params, self.platforms.has_sky_audio());
            }
            self.approvals.consume_audio()?;
            return self.execute_inner(
                "audio.start",
                &json!({"scope":"system","max_duration_ms":params["max_duration_ms"]}),
            );
        }
        if method == "stop_audio_recording" {
            let audio = if self.desktop.sky_target() == "linux" {
                self.execute_audio("stop", &params, self.platforms.has_sky_audio())?
            } else {
                self.execute_inner("audio.stop", &json!({}))?
            };
            let file = self.media.write(string(&audio, "data")?, "audio/wav")?;
            return Ok(json!({"filepath":file["filepath"],"data_url":file["data_url"]}));
        }
        if self.desktop.sky_target() != "mac" {
            if self.desktop.sky_target() == "windows"
                && !["list_apps", "list_windows"].contains(&method)
            {
                let target = self.desktop.sky_policy_target(method, &params)?;
                self.approvals.authorize_app(&target.path)?;
            }
            let output = self.desktop.sky_execute(method, &params)?;
            if method != "get_screenshot" {
                return Ok(output);
            }
            let images = output
                .as_array()
                .ok_or_else(|| Error::action("Screenshot provider did not return an array"))?;
            let mut files = Vec::with_capacity(images.len());
            for image in images {
                let file = self
                    .media
                    .write(string(image, "data")?, string(image, "mime_type")?)?;
                files.push(json!({"filepath":file["filepath"],"data_url":file["data_url"]}));
            }
            return Ok(json!(files));
        }
        if method == "list_apps" {
            return self.desktop.sky_apps();
        }
        self.approvals.authorize_app(string(&params, "app")?)?;
        if method == "get_app_state" {
            let mut capture = params.clone();
            capture["screenshot"] = json!(self.desktop.capabilities().contains(&"get_screenshot"));
            let state = self.execute_inner("get_app_state", &capture)?;
            let screenshot = (|| -> Result<Value> {
                if state["screenshot"].is_null() {
                    return Ok(Value::Null);
                }
                let image = &state["screenshot"];
                let file = self
                    .media
                    .write(string(image, "data")?, string(image, "mime_type")?)?;
                Ok(json!({"url":file["url"]}))
            })();
            if screenshot.is_err()
                && let Some(app) = self.apps.get(state["app"].as_str().unwrap_or(""))
            {
                self.desktop.invalidate_screenshot(app);
            }
            let screenshot = screenshot?;
            let mut result = json!({"app":{"bundleIdentifier":state["bundleIdentifier"]},"skyshot":{"text":state["state"],"screenshot":screenshot}});
            if let Some(instructions) = state.get("appSpecificInstructions") {
                result["appSpecificInstructions"] = instructions.clone();
            }
            return Ok(result);
        }
        self.execute_inner(method, &params)
    }
    pub fn prepare_elicitation(&mut self, request: &Value) -> Result<(Value, Option<Value>)> {
        self.approvals.prepare_for_platform(
            request,
            &self.security,
            self.desktop.synthetic(),
            self.desktop.sky_target(),
        )
    }
    pub fn resolve_elicitation(&mut self, request: &Value, response: &Value) -> Result<Value> {
        self.approvals.resolve(request, response)
    }
    pub fn native_request(&mut self, args: &Value) -> Result<Value> {
        if args["clientApiVersion"] != IPC_VERSION {
            return Err(Error::new(-32001, "Incompatible client API version"));
        }
        if let Some(deadline) = args["deadlineUnixMilliseconds"].as_u64() {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            if now >= deadline as u128 {
                return Err(Error::new(-32002, "Request deadline exceeded"));
            }
        }
        let request = &args["request"];
        match string(args, "requestType")? {
            "ComputerUseIPCListAppsRequest" => self.execute("list_apps", &json!({})),
            "ComputerUseIPCAppGetSkyshotRequest" => self.execute("get_app_state", request),
            "ComputerUseIPCAppStartRequest" => self.execute("bind_app", request),
            "ComputerUseIPCAppPerformActionRequest" => {
                let (method, params) = decode_native_action(request)?;
                self.execute(method, &params)
            }
            // Decisions depend on this service's own policy; do not impersonate
            // the original sender-signing or account approval authority.
            "ComputerUseIPCStartAudioRecordingRequest" => self.execute("audio.start",&json!({"scope":"system","max_duration_ms":request.get("maxDurationMilliseconds").cloned().unwrap_or(json!(60000))})),
            "ComputerUseIPCStopAudioRecordingRequest" => self.execute("audio.stop",&json!({})),
            "ComputerUseIPCAppPolicyRequest" => {
                self.app(string(request, "app")?)?;
                Ok(json!({"policy":"local-owner","requiresAccessibilityPermission":true}))
            }
            other => Err(Error::unsupported(format!(
                "Native request not implemented: {other}"
            ))),
        }
    }
}
pub fn string<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args[key]
        .as_str()
        .ok_or_else(|| Error::invalid(format!("{key} must be a string")))
}
pub fn index(args: &Value) -> Result<u64> {
    args.get("element_index")
        .or_else(|| args.get("elementIndex"))
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::invalid("element_index must be a nonnegative integer"))
}
pub fn point(args: &Value, key: &str, x: &str, y: &str) -> Result<[f64; 2]> {
    let p = if let Some(a) = args[key].as_array() {
        if a.len() != 2 {
            return Err(Error::invalid("Point must have two coordinates"));
        }
        [a[0].as_f64(), a[1].as_f64()]
    } else if let Some(p) = args[key].as_object() {
        [
            p.get("x").and_then(Value::as_f64),
            p.get("y").and_then(Value::as_f64),
        ]
    } else {
        [args[x].as_f64(), args[y].as_f64()]
    };
    match p {
        [Some(x), Some(y)] if x.is_finite() && y.is_finite() => Ok([x, y]),
        _ => Err(Error::invalid("Point requires finite coordinates")),
    }
}

pub fn decode_native_action(request: &Value) -> Result<(&'static str, Value)> {
    let actions = request["action"]
        .as_object()
        .filter(|o| o.len() == 1)
        .ok_or_else(|| Error::invalid("Expected one action variant"))?;
    let (kind, a) = actions.iter().next().unwrap();
    let mut params = a.as_object().cloned().unwrap_or_default();
    params.insert("app".into(), request["app"].clone());
    let method = match kind.as_str() {
        "click" => "click",
        "drag" => "drag",
        "paste" => "paste",
        "performSecondaryAction" => "perform_secondary_action",
        "pressKey" => {
            params.insert("key".into(), a["_0"].clone());
            "press_key"
        }
        "scroll" => "scroll",
        "setValue" => "set_value",
        "selectText" => "select_text",
        "type" => {
            params.insert("text".into(), a["_0"].clone());
            "type_text"
        }
        _ => return Err(Error::invalid("Unknown action variant")),
    };
    if let Some(id) = a.get("elementID").or_else(|| a.pointer("/at/elementID/_0")) {
        let parsed = id
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| Error::invalid("Invalid element ID"))?;
        params.insert("element_index".into(), json!(parsed));
    }
    if let Some(p) = a.pointer("/at/coordinate/_0") {
        params.insert("point".into(), p.clone());
    }
    Ok((method, Value::Object(params)))
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.end_session();
    }
}
