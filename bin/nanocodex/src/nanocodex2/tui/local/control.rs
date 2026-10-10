//! tui-control for the local TUI, registered as kind "native" (legacy
//! L/control.rs): history.list and history.read page the local rollout with
//! stable byte cursors pinned to its committed boundary; prompt, steer, cancel
//! and settings.set drive the local agent; models.list returns the local catalog.

use nanocodex::{HarnessModel, ModelTransport, Thinking};
use nanocodex_tui_control::{Bridge, Command, Conversation, accepted, rejected, unknown};
use serde_json::{Value, json};
use tokio::task::JoinSet;

use super::super::{
    AppEvent, AppNode, ComponentUpdate, DriverRuntime, PaneId, Submission, TurnId,
    components::AppEffect, control::Completion,
};

/// The local model catalog (legacy `models()`).
fn models() -> Value {
    json!({"models": HarnessModel::for_family(nanocodex::HarnessFamily::Codex)
        .chain(HarnessModel::for_family(nanocodex::HarnessFamily::Claude))
        .map(|model| {
            let capabilities = model.capabilities(ModelTransport::Native);
            json!({
                "id": model.as_str(),
                "efforts": capabilities.thinking().map(|effort| effort.as_str()).collect::<Vec<_>>(),
                "fast_mode": capabilities.fast_mode(),
                "service_tiers": capabilities.service_tiers().map(|tier| tier.as_str()).collect::<Vec<_>>(),
                "reasoning_modes": capabilities.reasoning_modes().map(|mode| mode.as_str()).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>()})
}

/// Publishes the local conversation with its rollout so clients can read history.
pub(in crate::nanocodex2::tui) fn publish(bridge: &Bridge, runtime: &DriverRuntime) {
    let Some(agent) = runtime.agent.as_ref() else {
        return;
    };
    if runtime.agent_id.is_empty() {
        return;
    }
    let rollout = agent
        .persistence()
        .and_then(|persistence| persistence.rollout);
    if let Some(rollout) = &rollout {
        bridge.committed(&runtime.agent_id, rollout.committed_bytes());
    }
    bridge.conversation(Conversation {
        session_id: runtime.agent_id.clone(),
        root_session_id: Some(runtime.agent_id.clone()),
        parent_session_id: None,
        origin: "root".into(),
        role: "root".into(),
        rollout_path: rollout.map(|rollout| rollout.path().to_path_buf()),
    });
}

/// Handles one control command for a local session. Returns the transcript
/// update of an admitted prompt for the driver to apply.
pub(in crate::nanocodex2::tui) fn dispatch(
    command: Command,
    bridge: &Bridge,
    runtime: &mut DriverRuntime,
    app: &mut AppNode,
    tasks: &mut JoinSet<Completion>,
) -> Option<ComponentUpdate<AppEffect>> {
    let method = command.request.method.clone();
    if method == "models.list" {
        command.finish(models());
        return None;
    }
    let expected = command.request.params["expected_session_id"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    if matches!(method.as_str(), "history.list" | "history.read") {
        // Reads are independent of composer focus and turn state.
        if expected != runtime.agent_id {
            command.reject("session_changed");
            return None;
        }
        let Some(rollout) = runtime.agent.as_ref().and_then(|agent| {
            agent
                .persistence()
                .and_then(|persistence| persistence.rollout)
        }) else {
            command.reject("history_unavailable");
            return None;
        };
        let path = rollout.path().to_path_buf();
        let boundary = rollout.committed_bytes();
        bridge.committed(&runtime.agent_id, boundary);
        let session = runtime.agent_id.clone();
        tasks.spawn(async move {
            let params = command.request.params.clone();
            let read = tokio::task::spawn_blocking(move || -> std::io::Result<Value> {
                let file = std::fs::File::open(path)?;
                if method == "history.read" {
                    nanocodex_tui_control::history::chunk(file, boundary, &params)
                } else {
                    nanocodex_tui_control::history::page(file, boundary, &params)
                }
            })
            .await;
            let value = match read {
                Ok(Ok(value)) => value,
                Ok(Err(error)) => json!({"status":"rejected","code":"history_unavailable","message":error.to_string()}),
                Err(error) => unknown(error),
            };
            (command, value, None, session)
        });
        return None;
    }
    if let Err(code) = bridge.validate(&command.request) {
        command.reject(code);
        return None;
    }
    if expected != runtime.agent_id {
        command.reject("session_changed");
        return None;
    }
    let params = command.request.params.clone();
    let turn_id = params["expected_turn_id"].as_str().unwrap_or("").to_owned();
    let local_turn = runtime
        .local_managed_turns
        .iter()
        .find(|(local, managed)| {
            **managed == turn_id && !runtime.local_terminal_turns.contains(*local)
        })
        .map(|(local, _)| *local);
    match method.as_str() {
        "prompt" => {
            if !runtime.controls.is_empty()
                || !runtime.admitting.is_empty()
                || runtime.pending_submission.is_some()
            {
                command.reject("busy");
                return None;
            }
            let text = params["input"]["text"].as_str().unwrap_or("").to_owned();
            if text.trim().is_empty() {
                command.reject("empty_input");
                return None;
            }
            let prompt = Submission::text(text);
            let id = TurnId::new(runtime.next_turn);
            runtime.next_turn = runtime.next_turn.saturating_add(1);
            let record = match runtime.record_submission(id, &prompt) {
                Ok(record) => record,
                Err(error) => {
                    command.finish(unknown(error));
                    return None;
                }
            };
            let update = app.update(AppEvent::Transcript {
                pane: PaneId::Main,
                record,
            });
            runtime.start_submission(PaneId::Main, id, prompt);
            match runtime.local_managed_turns.get(&id) {
                Some(turn) => command.finish(accepted(json!({"turn_id": turn}))),
                // Waiting for the agent or a shell: the prompt is queued and admitted later.
                None => command.finish(accepted(json!({"queued": true}))),
            }
            Some(update)
        }
        "steer" | "cancel" => {
            let Some(control) = local_turn.and_then(|id| runtime.controls.get(&id).cloned()) else {
                command.reject("turn_not_active");
                return None;
            };
            let session = runtime.agent_id.clone();
            tasks.spawn(async move {
                let result = if command.request.method == "steer" {
                    let input = command.request.params["input"]["text"]
                        .as_str()
                        .unwrap_or("")
                        .to_owned();
                    if input.trim().is_empty() {
                        rejected("empty_input")
                    } else {
                        match control
                            .steer_with_id(command.request.id.clone(), input)
                            .await
                        {
                            Ok(()) => accepted(json!({"turn_id": turn_id})),
                            Err(nanocodex::NanocodexError::TurnNotSteerable) => {
                                rejected("turn_not_active")
                            }
                            Err(error) => unknown(error),
                        }
                    }
                } else {
                    match control.cancel().await {
                        Ok(()) => accepted(json!({"turn_id": turn_id})),
                        Err(error) => unknown(error),
                    }
                };
                (command, result, None, session)
            });
            None
        }
        "settings.set" => {
            let Some(agent) = runtime.agent.clone() else {
                command.reject("unavailable");
                return None;
            };
            let settings = params["settings"].clone();
            let mut current = runtime.settings;
            let session = runtime.agent_id.clone();
            let bridge = bridge.clone();
            tasks.spawn(async move {
                let outcome = if settings.as_object().is_none_or(|value| value.len() != 1) {
                    Err("set exactly one setting".to_owned())
                } else if let Some(effort) = settings["effort"].as_str() {
                    match effort.parse::<Thinking>() {
                        Ok(thinking) => agent
                            .set_thinking(thinking)
                            .await
                            .map(|()| current.thinking = thinking)
                            .map_err(|error| error.to_string()),
                        Err(error) => Err(error),
                    }
                } else if let Some(model) = settings["model"].as_str() {
                    match model.parse::<HarnessModel>() {
                        Ok(model) => agent
                            .set_harness_model(model)
                            .await
                            .map(|()| {
                                if let HarnessModel::Codex(model) = model {
                                    current.model = nanocodex_managed::ManagedModel::Oai(model);
                                }
                            })
                            .map_err(|error| error.to_string()),
                        Err(error) => Err(error.to_string()),
                    }
                } else {
                    Err("unsupported setting".to_owned())
                };
                match outcome {
                    Ok(()) => {
                        bridge.settings_committed(settings.clone());
                        (
                            command,
                            accepted(json!({"settings": settings})),
                            Some(current),
                            session,
                        )
                    }
                    Err(message) => (
                        command,
                        json!({"status":"rejected","code":"invalid_settings","message":message}),
                        None,
                        session,
                    ),
                }
            });
            None
        }
        _ => {
            command.reject("unsupported_method");
            None
        }
    }
}
