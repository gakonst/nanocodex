//! The response channel belongs exclusively to a local AppKit dialog. Never expose
//! a tool/API that supplies a decision; dropping the pending future kills its UI.
use super::{Action, ComputerElicitationRequest, MAX_FORM_BYTES, Response, Scope, ToolError};
use std::{os::unix::fs::MetadataExt, process::Stdio};

pub(super) fn available() -> bool {
    std::fs::metadata("/dev/console").is_ok_and(|metadata| {
        metadata.uid() != 0 && metadata.uid() == nix::unistd::geteuid().as_raw()
    })
}

pub(super) async fn review(request: ComputerElicitationRequest) -> Result<Response, ToolError> {
    let schema = request
        .params
        .get("requestedSchema")
        .ok_or("Missing form schema")?;
    let validator = jsonschema::validator_for(schema).map_err(|_| "Unsupported form schema")?;
    let form = serde_json::to_string_pretty(&request.params)?;
    if form.len() > MAX_FORM_BYTES {
        return Err("CUA consent form exceeds native review limit".into());
    }
    let session_allowed = Scope::from_request(&request).is_some();
    let payload = serde_json::json!({
        "form": form,
        "context": request.context.as_ref().map(|context| serde_json::json!({
            "session_id":context.session_id,"call_id":context.call_id,"model":context.model,
        })),
        "sessionAllowed": session_allowed,
    });
    let output = tokio::process::Command::new("/usr/bin/osascript")
        .args([
            "-l",
            "JavaScript",
            "-e",
            include_str!("computer_elicitation_macos.js"),
            &payload.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await?;
    if !output.status.success() {
        return Ok(super::cancel());
    }
    let result: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let action = result["action"]
        .as_str()
        .ok_or("Invalid native consent response")?;
    // Keep validation, persistence constraints, and content semantics shared with
    // the terminal path. Invalid input fails closed, never supplying defaults.
    let line = match action {
        "accept" | "accept-session" => format!(
            "{action} {}",
            result["content"].as_str().ok_or("Missing form content")?
        ),
        "decline" | "cancel" => action.to_owned(),
        _ => return Err("Invalid native consent response".into()),
    };
    let response = super::decision(&line, "native-dialog", &validator, session_allowed)?;
    if response.action == Action::Accept && request.provider_session.upgrade().is_none() {
        return Ok(super::cancel());
    }
    Ok(response)
}
