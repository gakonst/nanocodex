//! Native owner for cooperative origin admission. Commands remain stored native
//! data; only opaque IDs cross the facade. A finished command is never reissued.
use crate::{Error, Result, browser::Browsers, engine::Engine, origin_elicitation::Ticket};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
#[derive(Default)]
pub(crate) struct Owner {
    cell: Option<(String, u64)>,
    ticket: Option<Ticket>,
    operations: BTreeMap<String, Operation>,
    queues: BTreeMap<String, VecDeque<String>>,
}
struct Operation {
    method: String,
    args: Value,
    snapshot: Option<Value>,
    origins: VecDeque<String>,
    approval: Option<(String, String)>,
    accepted: Vec<String>,
    // Immutable refusal-only native data; never retain ProviderControl,
    // execution validity, suspension or approval authority on an operation.
    activation_model: Option<crate::browser_activation::Model>,
}
pub(crate) fn exempt(method: &str, args: &Value) -> bool {
    [
        "list",
        "info",
        "get_browser",
        "get_default_browser",
        "get_browser_for_url",
        "documentation",
        "list_tabs",
        "get_tab",
        "selected_tab",
        "close_tab",
        "mark_tab",
        "name_session",
        "user_open_tabs",
        "user_claim_tab",
        "wait_for_timeout",
        "navigation_arm",
        "navigation_poll",
        "navigation_cancel",
    ]
    .contains(&method)
        || method == "new_tab" && args.get("url").is_none()
        || args.get("tab").is_none()
            && !matches!(
                method,
                "navigate" | "new_tab" | "tabs_content" | "user_history" | "cdp_call"
            )
}
fn retained_bytes(args: &Value, model: Option<&crate::browser_activation::Model>) -> Result<usize> {
    let metadata = model
        .map(serde_json::to_vec)
        .transpose()?
        .map_or(0, |bytes| bytes.len());
    Ok(serde_json::to_vec(args)?.len().saturating_add(metadata))
}
fn key(args: &Value) -> String {
    json!([args["browser"], args["tab"]]).to_string()
}
impl Owner {
    pub fn begin(&mut self, scope: &str, cell: u64) {
        self.cancel_operations();
        self.cell = Some((scope.into(), cell));
    }
    pub fn finish(&mut self, scope: &str, cell: u64) {
        if self
            .cell
            .as_ref()
            .is_some_and(|c| c.0 == scope && c.1 == cell)
        {
            self.clear();
        }
    }
    pub fn reset(&mut self, scope: &str) {
        if self.cell.as_ref().is_some_and(|c| c.0 == scope) {
            self.clear();
        }
    }
    pub fn clear(&mut self) {
        self.cancel_operations();
        self.cell = None;
        if let Some(ticket) = self.ticket.take() {
            ticket.finish();
        }
    }
    fn cancel_operations(&mut self) {
        if let Some(ticket) = &self.ticket {
            for operation in self.operations.values() {
                if let Some((id, _)) = &operation.approval {
                    ticket.cancel(id);
                }
            }
        }
        self.operations.clear();
        self.queues.clear();
    }
    pub fn waiting_on(&self, args: &Value) -> bool {
        self.queues
            .get(&key(args))
            .is_some_and(|queue| !queue.is_empty())
    }
    pub fn close(&mut self, args: &Value) {
        if let Some(queue) = self.queues.remove(&key(args)) {
            for id in queue {
                self.remove(&id);
            }
        }
    }
    fn remove(&mut self, id: &str) {
        if let Some(operation) = self.operations.remove(id) {
            if let Some((prompt, _)) = &operation.approval
                && let Some(ticket) = &self.ticket
            {
                ticket.cancel(prompt);
            }
            let key = key(&operation.args);
            if let Some(queue) = self.queues.get_mut(&key) {
                queue.retain(|item| item != id);
                if queue.is_empty() {
                    self.queues.remove(&key);
                }
            }
        }
    }
    fn validate(&self, scope: &str) -> Result<&Ticket> {
        self.cell
            .as_ref()
            .filter(|(owner_scope, _)| owner_scope == scope)
            .ok_or_else(|| {
                Error::new(
                    -32800,
                    "Browser operation has no active cell in this kernel scope",
                )
            })?;
        let ticket = self
            .ticket
            .as_ref()
            .ok_or_else(|| Error::new(-32011, "Origin approval broker is unavailable"))?;
        ticket.validate()?;
        Ok(ticket)
    }
}
impl Engine {
    /// Native runtime metadata may admit a bounded human-only drain slice. The
    /// model's request JSON supplies neither phase proof nor continuation grants.
    pub fn execute_from_js_controlled(
        &mut self,
        method: &str,
        args: &Value,
        control: &crate::runtime::ProviderControl,
    ) -> Result<Value> {
        let result = self.execute_from_js_with_control(method, args, Some(control))?;
        if !matches!(
            method,
            "browser.origin_operation_start" | "browser.origin_operation_poll"
        ) || result["pending"] != true
        {
            return Ok(result);
        }
        let Some(id) = result["id"].as_str() else {
            return Ok(result);
        };
        let owner = &self.navigation_security;
        let ticket = owner.validate(self.selected_kernel_scope())?;
        if !owner.operations.contains_key(id) {
            return Err(Error::action("Native continuation is no longer pending"));
        }
        control.bind_continuation(id.to_string())?;
        if method == "browser.origin_operation_poll"
            && let Some(proof) = control.drain_proof()
        {
            proof.validate()?;
            let mut ids = proof.continuations;
            ids.push(id.to_string());
            for id in &ids {
                let Some(operation) = owner.operations.get(id) else {
                    return Ok(result);
                };
                if owner
                    .queues
                    .get(&key(&operation.args))
                    .and_then(|queue| queue.front())
                    != Some(id)
                {
                    return Ok(result);
                }
                let Some((prompt, _)) = &operation.approval else {
                    return Ok(result);
                };
                if !ticket.human_ready(prompt)? {
                    return Ok(result);
                }
            }
            let (prompt, _) = owner.operations[id].approval.as_ref().unwrap();
            ticket.wait_slice(prompt, control)?;
        }
        Ok(result)
    }
    pub(crate) fn beforeunload_runtime_admission(
        &self,
        method: &str,
        args: &Value,
        control: &crate::runtime::ProviderControl,
    ) -> Option<crate::browser::beforeunload::RuntimeAdmission> {
        use crate::browser::beforeunload::{Kind, RuntimeAdmission};
        if self.security.browser_restricted()
            || self.guardian_monitor.is_some()
            || self.host_turns.is_some()
            || args.get("frame").is_some()
        {
            return None;
        }
        let kind = match method {
            "navigate" => Kind::Navigate,
            "close_tab" => Kind::Close,
            _ => return None,
        };
        let ticket = self
            .navigation_security
            .validate(self.selected_kernel_scope())
            .ok()?;
        RuntimeAdmission::new(
            args["browser"].as_str()?,
            args["tab"].as_str()?,
            kind,
            control.execution_validity()?,
            ticket.connection_liveness()?,
        )
    }
    pub fn set_origin_approval(&mut self, ticket: Option<Ticket>) {
        if let Some(previous) = self.navigation_security.ticket.take() {
            previous.finish();
        }
        self.navigation_security.ticket = ticket;
    }
    fn origin_snapshot(&mut self, method: &str, args: &Value) -> Result<(Value, VecDeque<String>)> {
        self.security.check_browser_command(method)?;
        self.security.check_browser_frame(method, args)?;
        let scope = self.selected_kernel_scope().to_string();
        let mut origins = VecDeque::new();
        if matches!(method, "navigate" | "new_tab") {
            let url = args["url"]
                .as_str()
                .ok_or_else(|| Error::invalid("Navigation requires a URL"))?;
            if let Some(origin) = self.security.origin_access(&scope, url)? {
                origins.push_back(origin);
            }
        }
        let mut main_args = args.clone();
        main_args
            .as_object_mut()
            .ok_or_else(|| Error::invalid("Browser arguments must be an object"))?
            .remove("frame");
        let main = self.browsers.authorization_context(&main_args)?;
        let target = if args.get("frame").is_some() {
            self.browsers.authorization_context(args)?
        } else {
            main.clone()
        };
        if !matches!(method, "navigate" | "new_tab") {
            for context in [&main, &target] {
                let url = context["url"]
                    .as_str()
                    .ok_or_else(|| Error::new(-32014, "Browser document URL is unavailable"))?;
                if let Some(origin) = self.security.origin_access(&scope, url)?
                    && !origins.contains(&origin)
                {
                    origins.push_back(origin);
                }
            }
        }
        Ok((json!({"main":main,"target":target}), origins))
    }
    pub(crate) fn check_direct_origin_access(
        &mut self,
        method: &str,
        args: &mut Value,
        admitted: bool,
    ) -> Result<()> {
        if exempt(method, args) {
            return Ok(());
        }
        if !admitted && self.navigation_security.waiting_on(args) {
            return Err(Error::new(
                -32014,
                "Browser navigation authorization is pending",
            ));
        }
        let (snapshot, origins) = self.origin_snapshot(method, args)?;
        if !origins.is_empty() {
            return Err(Error::new(
                -32011,
                "Browser origin access requires host approval",
            ));
        }
        if crate::security::Security::browser_document_bound(method) {
            let context = &snapshot["target"];
            for (expected, observed) in [
                ("expectedDocumentToken", "documentToken"),
                ("expectedUrl", "url"),
            ] {
                if args.get(expected).is_some_and(|v| v != &context[observed]) {
                    return Err(Error::new(
                        -32014,
                        "Document changed before origin-bound action",
                    ));
                }
                args[expected] = context[observed].clone();
            }
            args["frame"] = context["frameId"].clone();
        }
        Ok(())
    }
    pub(crate) fn start_origin_operation(
        &mut self,
        args: &Value,
        control: Option<&crate::runtime::ProviderControl>,
    ) -> Result<Value> {
        if !self.security.origin_approval_required() {
            return Err(Error::unsupported(
                "Origin operation channel is not enabled",
            ));
        }
        let method = args["method"]
            .as_str()
            .ok_or_else(|| Error::invalid("Browser operation method is required"))?;
        if method.starts_with("origin_operation_") {
            return Err(Error::invalid("Nested browser origin operation"));
        }
        let (method, params) = Browsers::normalize_request(method, &args["args"])?;
        if method == "cdp_events" && params.get("__skyreRawWait").is_some() {
            return Err(Error::invalid(
                "Raw event continuations cannot start a new origin operation",
            ));
        }
        self.security.check_browser_command(&method)?;
        self.security.check_browser_frame(&method, &params)?;
        if matches!(method.as_str(), "navigate" | "new_tab")
            && let Some(url) = params["url"].as_str()
        {
            self.security.check_url(url)?;
        }
        let activation_model = if crate::browser_activation::command(&method).is_some() {
            Some(
                control
                    .map(|control| control.activation_model().cloned())
                    .transpose()?
                    .unwrap_or_default(),
            )
        } else {
            None
        };
        if self.navigation_security.ticket.is_none() {
            let value = self.execute_browser_with_model(
                &method,
                &params,
                false,
                activation_model.as_ref(),
            )?;
            return Ok(json!({"pending": false, "value": value}));
        }
        self.navigation_security
            .validate(self.selected_kernel_scope())?;
        if method == "close_tab"
            || exempt(&method, &params) && !self.navigation_security.waiting_on(&params)
        {
            let value =
                self.execute_browser_with_model(&method, &params, true, activation_model.as_ref())?;
            return Ok(json!({"pending":false,"value":value}));
        }
        let retained_total = self.navigation_security.operations.values().try_fold(
            0usize,
            |sum, operation| -> Result<usize> {
                Ok(sum.saturating_add(retained_bytes(
                    &operation.args,
                    operation.activation_model.as_ref(),
                )?))
            },
        )?;
        if retained_total.saturating_add(retained_bytes(&params, activation_model.as_ref())?)
            > 4 * 1024 * 1024
        {
            return Err(Error::action(
                "Pending browser operation arguments exceed 4 MiB",
            ));
        }
        if self.navigation_security.operations.len() >= 128 {
            return Err(Error::action("Too many pending browser operations"));
        }
        let mut nonce = [0u8; 32];
        getrandom::fill(&mut nonce)
            .map_err(|_| Error::action("Cannot generate browser operation ID"))?;
        let id = nonce.iter().map(|b| format!("{b:02x}")).collect::<String>();
        self.navigation_security
            .queues
            .entry(key(&params))
            .or_default()
            .push_back(id.clone());
        self.navigation_security.operations.insert(
            id.clone(),
            Operation {
                method,
                args: params,
                snapshot: None,
                origins: VecDeque::new(),
                approval: None,
                accepted: vec![],
                activation_model,
            },
        );
        self.advance_origin_operation(&id)
    }
    pub(crate) fn poll_origin_operation(&mut self, args: &Value) -> Result<Value> {
        let id = args["id"]
            .as_str()
            .ok_or_else(|| Error::invalid("Browser operation ID is required"))?;
        self.advance_origin_operation(id)
    }
    fn advance_origin_operation(&mut self, id: &str) -> Result<Value> {
        // Keep only failure context across admission; the operation itself is
        // consumed before dispatch so provider errors cannot make it replayable.
        let navigation_key = self
            .navigation_security
            .operations
            .get(id)
            .filter(|operation| operation.method == "navigate")
            .map(|operation| key(&operation.args));
        let result = self.advance_origin_operation_inner(id);
        if result.is_err()
            && let Some(tab_key) = navigation_key
        {
            let dependent: Vec<_> = self
                .navigation_security
                .operations
                .iter()
                .filter(|(other, operation)| {
                    *other != id
                        && key(&operation.args) == tab_key
                        && operation.method != "navigate"
                })
                .map(|(id, _)| id.clone())
                .collect();
            for dependent in dependent {
                self.navigation_security.remove(&dependent);
            }
        }
        if !matches!(&result,Ok(value) if value["pending"]==true) {
            self.navigation_security.remove(id);
        }
        result
    }
    fn advance_origin_operation_inner(&mut self, id: &str) -> Result<Value> {
        self.navigation_security
            .validate(self.selected_kernel_scope())?;
        let operation =
            self.navigation_security.operations.get(id).ok_or_else(|| {
                Error::new(-32800, "Browser operation ended or was already consumed")
            })?;
        if self
            .navigation_security
            .queues
            .get(&key(&operation.args))
            .and_then(|q| q.front())
            .map(String::as_str)
            != Some(id)
        {
            return Ok(json!({"pending":true,"id":id}));
        }
        let (method, mut args, snapshot, activation_model) = (
            operation.method.clone(),
            operation.args.clone(),
            operation.snapshot.clone(),
            operation.activation_model.clone(),
        );
        if snapshot.is_none() && !exempt(&method, &args) {
            let (snapshot, origins) = self.origin_snapshot(&method, &args)?;
            let operation = self.navigation_security.operations.get_mut(id).unwrap();
            operation.snapshot = Some(snapshot);
            operation.origins = origins;
        }
        let approval = self.navigation_security.operations[id].approval.clone();
        if let Some((prompt, origin)) = approval {
            let Some(reply) = self
                .navigation_security
                .validate(self.selected_kernel_scope())?
                .poll(&prompt)?
            else {
                return Ok(json!({"pending":true,"id":id}));
            };
            if reply["action"] != "accept" {
                return Err(Error::new(-32012, "Browser origin access was not approved"));
            }
            let operation = self.navigation_security.operations.get_mut(id).unwrap();
            operation.approval = None;
            operation.accepted.push(origin);
        }
        if let Some(origin) = self
            .navigation_security
            .operations
            .get_mut(id)
            .unwrap()
            .origins
            .pop_front()
        {
            let prompt = self
                .navigation_security
                .validate(self.selected_kernel_scope())?
                .begin(&origin)?;
            self.navigation_security
                .operations
                .get_mut(id)
                .unwrap()
                .approval = Some((prompt, origin));
            return Ok(json!({"pending":true,"id":id}));
        }
        if let Some(snapshot) = self.navigation_security.operations[id].snapshot.clone() {
            let (current, _) = self.origin_snapshot(&method, &args)?;
            if current != snapshot {
                return Err(Error::new(
                    -32014,
                    "Browser document or provider changed during origin approval",
                ));
            }
            if crate::security::Security::browser_document_bound(&method) {
                // Carry the approved document into the checked dispatch. A
                // later policy read must reject replacement, never rebind it.
                let context = &snapshot["target"];
                for (expected, observed) in [
                    ("expectedDocumentToken", "documentToken"),
                    ("expectedUrl", "url"),
                ] {
                    if args
                        .get(expected)
                        .is_some_and(|value| value != &context[observed])
                    {
                        return Err(Error::new(
                            -32014,
                            "Document changed before origin-bound action",
                        ));
                    }
                    args[expected] = context[observed].clone();
                }
                args["frame"] = context["frameId"].clone();
            }
        }
        // Native admission point: cancellation before this check retires the
        // operation. Once dispatched, a transport error never causes replay.
        self.navigation_security
            .validate(self.selected_kernel_scope())?;
        let accepted = self.navigation_security.operations[id].accepted.clone();
        let scope = self.selected_kernel_scope().to_string();
        for origin in accepted {
            self.security.grant_origin_access(&scope, &origin)?;
        }
        self.navigation_security.remove(id);
        let value =
            self.execute_browser_with_model(&method, &args, true, activation_model.as_ref())?;
        Ok(json!({"pending":false,"value":value}))
    }
}

#[cfg(test)]
#[path = "browser_activation_origin_tests.rs"]
mod activation_tests;
