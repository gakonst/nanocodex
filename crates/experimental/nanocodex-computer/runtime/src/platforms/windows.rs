//! Recovered Windows client argument lowering and independent persistent helper host.
use super::{process::Lines, required};
use crate::{Error, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};
const INTERRUPTED: &str = "Computer Use was stopped by the user with the physical Escape key";
pub struct Windows {
    executable: PathBuf,
    args: Vec<String>,
    timeout: Duration,
    child: Option<Lines>,
    next_id: u64,
    turn: Option<(String, String)>,
    metadata: Value,
    interrupts: Vec<(String, String)>,
    state_directory: Option<PathBuf>,
    events: VecDeque<Value>,
}
impl Windows {
    pub fn new(
        executable: PathBuf,
        args: Vec<String>,
        timeout: Duration,
        state_directory: Option<PathBuf>,
    ) -> Result<Self> {
        if let Some(path) = &state_directory {
            if !path.is_absolute() {
                return Err(Error::invalid("state_directory must be absolute"));
            }
            if !path.exists() {
                #[cfg_attr(not(unix), allow(unused_mut))]
                let mut dir = fs::DirBuilder::new();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    dir.mode(0o700);
                }
                dir.create(path)?;
            }
            let meta = fs::symlink_metadata(path)?;
            if !meta.is_dir() || meta.file_type().is_symlink() {
                return Err(Error::invalid(
                    "Interrupt state must be a non-symlink directory",
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
                    return Err(Error::invalid(
                        "Interrupt state directory must be private and owned by current user",
                    ));
                }
            }
        }
        Ok(Self {
            executable,
            args,
            timeout,
            child: None,
            next_id: 1,
            turn: None,
            metadata: json!({}),
            interrupts: vec![],
            state_directory,
            events: VecDeque::new(),
        })
    }
    fn ensure_child(&mut self) -> Result<()> {
        if self.child.is_none() {
            self.child = Some(Lines::spawn(&self.executable, &self.args)?);
        }
        Ok(())
    }
    fn raw(&mut self, method: &str, params: &Value, meta: &Value) -> Result<Value> {
        self.ensure_child()?;
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| Error::action("Helper request ID exhausted"))?;
        let mut metadata = meta.clone();
        if !metadata.is_object() {
            return Err(Error::invalid("Windows request metadata must be an object"));
        }
        metadata["x-oai-cua-request-budget-ms"] = json!(self.timeout.as_millis());
        let request = json!({"id":id,"method":method,"params":params,"meta":metadata});
        let deadline = Instant::now() + self.timeout;
        let result = (|| -> Result<Value> {
            self.child.as_mut().unwrap().send(&request, self.timeout)?;
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(Error::new(-32008, "Windows helper request timed out"));
                }
                let response = self.child.as_mut().unwrap().receive(remaining)?;
                if response.get("id").and_then(Value::as_u64).is_none() {
                    self.events.push_back(response);
                    if self.events.len() > 1000 {
                        self.events.pop_front();
                    }
                    continue;
                }
                if response["id"].as_u64() != Some(id) {
                    continue;
                }
                if response.get("ok") == Some(&Value::Bool(true)) {
                    return Ok(response.get("result").cloned().unwrap_or(Value::Null));
                }
                if let Some(approval) = response.get("approvalRequest") {
                    let app = required(approval, "app")?;
                    return Ok(
                        json!({"approval_required":{"app":app,"displayName":approval.get("displayName").and_then(Value::as_str).unwrap_or(app),"allowPersistentApproval":approval.get("allowPersistentApproval").and_then(Value::as_bool).unwrap_or(true)&&app!="computer-audio","riskLevel":approval.get("riskLevel").and_then(Value::as_str).unwrap_or("low")},"dispatched":false}),
                    );
                }
                let message = response
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("Windows helper request failed");
                return Err(Error::new(
                    if message.starts_with(INTERRUPTED) {
                        -32010
                    } else {
                        -10005
                    },
                    message,
                ));
            }
        })();
        if let Err(error) = &result
            && error.code != -10005
        {
            self.child.take();
            if error.code == -32010 {
                self.mark_interrupted()?;
            }
        }
        result
    }
    pub fn execute(&mut self, method: &str, params: &Value, metadata: Value) -> Result<Value> {
        if method == "capabilities" {
            return Ok(
                json!({"target":"windows","native_backend":"explicit external helper","approval":"reported; never automatically accepted","methods":["activate_window","get_window_state","click","scroll","drag","press_key","type_text","launch_app","list_apps","list_windows","get_window","perform_secondary_action","set_value","start_audio_recording","stop_audio_recording","end_turn","close"]}),
            );
        }
        if method == "close" {
            self.end_turn()?;
            if self.child.is_some() {
                let meta = self.metadata.clone();
                let _ = self.raw("close", &json!({}), &meta);
            }
            self.child.take();
            return Ok(Value::Null);
        }
        if method == "end_turn" {
            return self.end_turn();
        }
        let turn = turn_scope(&metadata)?;
        if let Some(scope) = &turn
            && (self.interrupts.contains(scope) || self.marker(scope).is_some_and(|p| p.exists()))
        {
            return Err(Error::new(-32010, INTERRUPTED));
        }
        let (wire_method, wire_params) = lower(method, params)?;
        if self.turn.is_some() && self.turn != turn {
            let old_meta = self.metadata.clone();
            let _ = self.raw("end_turn", &json!({}), &old_meta);
        }
        self.turn = turn;
        self.metadata = metadata.clone();
        let result = self.raw(&wire_method, &wire_params, &metadata)?;
        if result.get("approval_required").is_some() {
            return Ok(result);
        }
        normalize_result(method, result)
    }
    pub fn end_turn(&mut self) -> Result<Value> {
        let result = if self.child.is_some() && self.turn.is_some() {
            let metadata = self.metadata.clone();
            self.raw("end_turn", &json!({}), &metadata)
        } else {
            Ok(Value::Null)
        };
        self.turn = None;
        self.metadata = json!({});
        result
    }
    pub fn drain_events(&mut self) -> Value {
        json!(self.events.drain(..).collect::<Vec<_>>())
    }
    fn marker(&self, scope: &(String, String)) -> Option<PathBuf> {
        self.state_directory.as_ref().map(|dir| {
            let hash = Sha256::digest(format!("{}\0{}", scope.0, scope.1));
            let name: String = hash.iter().map(|b| format!("{b:02x}")).collect();
            dir.join(format!("interrupted-{name}"))
        })
    }
    fn mark_interrupted(&mut self) -> Result<()> {
        if let Some(scope) = self.turn.clone() {
            if !self.interrupts.contains(&scope) {
                self.interrupts.push(scope.clone());
            }
            if let Some(path) = self.marker(&scope) {
                let mut options = OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                match options.open(path) {
                    Ok(mut f) => {
                        f.write_all(b"physical-escape\n")?;
                        f.sync_all()?;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                    Err(e) => return Err(e.into()),
                }
            }
        }
        Ok(())
    }
}
fn turn_scope(meta: &Value) -> Result<Option<(String, String)>> {
    if !meta.is_object() {
        return Err(Error::invalid("Metadata must be an object"));
    }
    match (meta.get("session_id"), meta.get("turn_id")) {
        (None, None) => Ok(None),
        (Some(session), Some(turn)) => {
            let s = session
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| Error::invalid("session_id must be nonempty"))?;
            let t = turn
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| Error::invalid("turn_id must be nonempty"))?;
            Ok(Some((s.into(), t.into())))
        }
        _ => Err(Error::invalid(
            "Both session_id and turn_id are required for turn ownership",
        )),
    }
}
fn int(v: &Value, label: &str) -> Result<u64> {
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
        .ok_or_else(|| Error::invalid(format!("{label} must be an integer >= 0")))
}
fn window(v: &Value) -> Result<Value> {
    let app = required(v, "app")?;
    let id = int(
        v.get("id")
            .ok_or_else(|| Error::invalid("window.id required"))?,
        "window.id",
    )?;
    let mut out = json!({"app":app,"id":id});
    if let Some(title) = v.get("title").and_then(Value::as_str) {
        out["title"] = json!(title);
    }
    Ok(out)
}
fn finite(v: Option<&Value>, label: &str) -> Result<Value> {
    let n = v
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite())
        .ok_or_else(|| Error::invalid(format!("{label} must be finite")))?;
    Ok(json!((n + 0.5).floor()))
}
/// Validated lowering mirrors the retained window-centric argument names.
pub fn lower(method: &str, params: &Value) -> Result<(String, Value)> {
    if !params.is_object() {
        return Err(Error::invalid("Windows parameters must be an object"));
    }
    let mut output = json!({});
    let mut command = method.to_string();
    match method {
        "list_apps" | "list_windows" | "stop_audio_recording" => (),
        "launch_app" => output["app"] = json!(required(params, "app")?),
        "start_audio_recording" => {
            if let Some(ms) = params.get("max_duration_ms") {
                let ms = int(ms, "max_duration_ms")?;
                if !(100..=300000).contains(&ms) {
                    return Err(Error::invalid("Audio duration must be 100..300000ms"));
                }
                output["max_duration_ms"] = json!(ms);
            }
        }
        "get_window" => {
            output["id"] = json!(int(
                params
                    .get("id")
                    .ok_or_else(|| Error::invalid("id required"))?,
                "id"
            )?);
            if let Some(app) = params.get("app") {
                output["app"] = json!(
                    app.as_str()
                        .filter(|s| !s.trim().is_empty())
                        .ok_or_else(|| Error::invalid("app must be nonempty"))?
                );
            }
        }
        "activate_window"
        | "get_window_state"
        | "click"
        | "scroll"
        | "drag"
        | "press_key"
        | "type_text"
        | "perform_secondary_action"
        | "set_value" => {
            output["window"] = window(
                params
                    .get("window")
                    .ok_or_else(|| Error::invalid("window required"))?,
            )?;
            match method {
                "get_window_state" => {
                    for (key, default) in [("include_screenshot", true), ("include_text", false)] {
                        output[key] = json!(
                            params
                                .get(key)
                                .map(|v| v.as_bool().ok_or_else(|| Error::invalid(format!(
                                    "{key} must be Boolean"
                                ))))
                                .transpose()?
                                .unwrap_or(default)
                        );
                    }
                    if output["include_screenshot"] == false && output["include_text"] == false {
                        return Err(Error::invalid(
                            "get_window_state must request text or screenshots",
                        ));
                    }
                }
                "click" => {
                    let count =
                        finite(params.get("click_count").or(Some(&json!(1))), "click_count")?;
                    if count.as_f64().unwrap() < 1.0 {
                        return Err(Error::invalid("click_count must be >= 1"));
                    }
                    output["click_count"] = count;
                    let button = params
                        .get("mouse_button")
                        .and_then(Value::as_str)
                        .unwrap_or("left");
                    if !["left", "right", "middle"].contains(&button) {
                        return Err(Error::invalid("Invalid mouse button"));
                    }
                    output["mouse_button"] = json!(button);
                    if let Some(element) = params
                        .get("element_index")
                        .or_else(|| params.get("elementIndex"))
                        .or_else(|| params.get("element"))
                    {
                        command = "click_element".into();
                        output["element_index"] = json!(int(element, "element_index")?);
                    } else {
                        for key in ["x", "y"] {
                            output[key] = finite(params.get(key), key)?;
                        }
                        copy_screenshot_id(params, &mut output)?;
                    }
                }
                "scroll" => {
                    for key in ["x", "y", "scrollX", "scrollY"] {
                        output[key] = finite(params.get(key), key)?;
                    }
                    copy_screenshot_id(params, &mut output)?;
                }
                "drag" => {
                    for key in ["from_x", "from_y", "to_x", "to_y"] {
                        output[key] = finite(params.get(key), key)?;
                    }
                    copy_screenshot_id(params, &mut output)?;
                }
                "press_key" => {
                    let key = required(params, "key")?
                        .split('+')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join("+");
                    if key.is_empty() {
                        return Err(Error::invalid("key required"));
                    }
                    output["key"] = json!(key);
                }
                "type_text" => {
                    output["text"] = json!(
                        params
                            .get("text")
                            .and_then(Value::as_str)
                            .ok_or_else(|| Error::invalid("text must be a string"))?
                    )
                }
                "set_value" | "perform_secondary_action" => {
                    output["element_index"] = json!(int(
                        params
                            .get("element_index")
                            .ok_or_else(|| Error::invalid("element_index required"))?,
                        "element_index"
                    )?);
                    let key = if method == "set_value" {
                        "value"
                    } else {
                        "action"
                    };
                    let value = params
                        .get(key)
                        .and_then(Value::as_str)
                        .ok_or_else(|| Error::invalid(format!("{key} must be a string")))?;
                    if key == "action" && value.trim().is_empty() {
                        return Err(Error::invalid("action required"));
                    }
                    output[key] = json!(value);
                }
                _ => (),
            }
        }
        _ => {
            return Err(Error::unsupported(format!(
                "Unsupported Windows method: {method}"
            )));
        }
    }
    Ok((command, output))
}
fn copy_screenshot_id(input: &Value, out: &mut Value) -> Result<()> {
    if let Some(id) = input.get("screenshotId") {
        let id = id
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::invalid("screenshotId must be a nonempty string"))?;
        out["screenshotId"] = json!(id);
    }
    Ok(())
}
fn normalize_result(method: &str, result: Value) -> Result<Value> {
    match method {
        "list_windows" => {
            let rows = result
                .as_array()
                .ok_or_else(|| Error::action("Windows helper did not return windows"))?;
            Ok(json!(
                rows.iter()
                    .filter_map(|v| window(v).ok())
                    .collect::<Vec<_>>()
            ))
        }
        "get_window" => window(&result),
        "get_window_state" => {
            if !result.is_object() {
                return Err(Error::action("Windows helper did not return window state"));
            }
            if let Some(ax) = result.get("accessibility")
                && !ax.is_null()
            {
                if !ax.get("tree").is_some_and(Value::is_string) {
                    return Err(Error::action(
                        "Windows helper did not return accessibility tree",
                    ));
                }
                for key in ["focused_element", "selected_text", "document_text"] {
                    if ax.get(key).is_some_and(|v| !v.is_null() && !v.is_string()) {
                        return Err(Error::action(format!("Invalid {key}")));
                    }
                }
                if ax.get("selected_elements").is_some_and(|v| {
                    !v.is_null() && !v.as_array().is_some_and(|a| a.iter().all(Value::is_string))
                }) {
                    return Err(Error::action("Invalid selected_elements"));
                }
            }
            if let Some(images) = result.get("screenshots") {
                let images = images
                    .as_array()
                    .ok_or_else(|| Error::action("Invalid screenshots"))?;
                for image in images {
                    required(image, "url")?;
                }
            }
            Ok(result)
        }
        _ => Ok(result),
    }
}

#[cfg(all(test, unix))]
mod deadline_tests {
    use super::*;

    #[test]
    fn platform_windows_ready_helper_deadline_restarts_child() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("ready-helper.py");
        std::fs::write(&script,r#"import sys,json,time,os
for line in sys.stdin:
 request=json.loads(line)
 if request['params'].get('text')=='slow':time.sleep(30)
 print(json.dumps({'id':request['id'],'ok':True,'result':{'ready':True,'pid':os.getpid()}}),flush=True)
"#).unwrap();
        let python = ["/usr/bin/python3", "/opt/homebrew/bin/python3"]
            .into_iter()
            .find(|p| std::path::Path::new(p).is_file())
            .expect("Python fixture interpreter unavailable");
        let mut provider = Windows::new(
            python.into(),
            vec![script.to_string_lossy().into_owned()],
            Duration::from_secs(10),
            None,
        )
        .unwrap();
        let metadata = json!({"session_id":"fixture","turn_id":"deadline"});
        // The completed reply establishes that Python imports, process setup and
        // its input loop are ready before measuring the short action deadline.
        let first = provider
            .execute("list_apps", &json!({}), metadata.clone())
            .unwrap();
        assert_eq!(first["ready"], true);
        provider.timeout = Duration::from_millis(250);
        let started = Instant::now();
        let error = provider
            .execute(
                "type_text",
                &json!({"window":{"app":"fixture","id":1},"text":"slow"}),
                metadata.clone(),
            )
            .unwrap_err();
        assert_eq!(error.code, -32008);
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(
            provider.child.is_none(),
            "Timeout must retire the failed generation"
        );
        provider.timeout = Duration::from_secs(10);
        let replacement = provider.execute("list_apps", &json!({}), metadata).unwrap();
        assert_eq!(replacement["ready"], true);
        assert_ne!(first["pid"], replacement["pid"]);
    }
}
