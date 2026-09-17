use super::*;
use nanocodex_tui_control::{Bridge, Command, Conversation, accepted, rejected, unknown};
use serde_json::{Value, json};

pub(super) type Completion = (Command, Value, Option<AgentSettings>, String);

pub(super) fn snapshot(bridge: &Bridge, app: &AppNode, runtime: &DriverRuntime, pending: bool) {
    let mut state = app
        .root(PaneId::Main)
        .map(RootNode::control_snapshot)
        .unwrap_or_else(|| json!({}));
    state["connection"] = json!(if runtime.pending_resume.is_some() {
        "switching"
    } else if runtime.recovery.is_some() {
        "reconnecting"
    } else if runtime.agent.is_none() {
        "connecting"
    } else if !runtime.managed_events_open {
        "disconnected"
    } else {
        "ready"
    });
    state["ui_blocked"] = json!(
        pending
            || state["ui_blocked"] == true
            || !runtime.settings_updates.is_empty()
            || !runtime.settings_queue.is_empty()
    );
    state["settings"] = json!({"model":runtime.settings.model.as_str(),"effort":runtime.settings.thinking.to_string(),
        "fast_mode":runtime.settings.fast_mode,"reasoning_mode":format!("{:?}",runtime.settings.reasoning_mode).to_lowercase()});
    state["active_turn_ids"] = json!(runtime.managed_active_turns.ids);
    state["managed_cursor"] = json!(runtime.observed_cursor);
    state["local_shells"] = json!(runtime.active_shells);
    if !runtime.agent_id.is_empty() {
        bridge.conversation(Conversation {
            session_id: runtime.agent_id.clone(),
            root_session_id: Some(runtime.agent_id.clone()),
            parent_session_id: None,
            origin: "root".into(),
            role: "root".into(),
            rollout_path: None,
        });
    }
    bridge.state(
        (!runtime.agent_id.is_empty()).then_some(runtime.agent_id.as_str()),
        state,
    );
}

fn outcome(result: Result<Value, ManagedError>) -> Value {
    match result {
        Ok(value) => accepted(value),
        Err(ManagedError::Http { code, .. }) if code == "command_delivery_unknown" => {
            unknown("service cannot establish delivery")
        }
        Err(ManagedError::Http { status, code, .. }) if status.is_client_error() => rejected(&code),
        Err(ManagedError::Configuration(message)) => {
            json!({"status":"rejected","code":"invalid_request","message":message})
        }
        Err(error) => unknown(error),
    }
}

pub(super) fn dispatch(
    command: Command,
    bridge: &Bridge,
    runtime: &DriverRuntime,
    tasks: &mut JoinSet<Completion>,
) {
    if command.request.method == "models.list" {
        command.finish(json!({"models":Model::ALL.into_iter().map(|model| json!({"id":model.as_str(),
            "efforts":nanocodex::Thinking::ALL.into_iter().filter(|effort|model.supports_thinking(*effort)).map(|effort|effort.to_string()).collect::<Vec<_>>()})).collect::<Vec<_>>()}));
        return;
    }
    if !matches!(
        command.request.method.as_str(),
        "history.list" | "command.status"
    ) && let Err(code) = bridge.validate(&command.request)
    {
        command.reject(code);
        return;
    }
    let agent_id = command.request.params["expected_session_id"]
        .as_str()
        .unwrap_or("");
    if agent_id != runtime.agent_id {
        command.reject("session_changed");
        return;
    }
    if command.request.method == "prompt"
        && (!runtime.controls.is_empty()
            || !runtime.managed_active_turns.ids.is_empty()
            || !runtime.admitting.is_empty()
            || runtime.pending_submission.is_some())
    {
        command.reject("busy");
        return;
    }
    let turn_id = command.request.params["expected_turn_id"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    if matches!(command.request.method.as_str(), "steer" | "cancel")
        && !runtime.managed_active_turns.ids.contains(&turn_id)
        && !runtime
            .local_managed_turns
            .values()
            .any(|id| id == &turn_id)
    {
        command.reject("turn_not_active");
        return;
    }
    let client = runtime.client.clone();
    let agent_id = runtime.agent_id.clone();
    tasks.spawn(async move {
        let mut updated = None;
        let p = &command.request.params;
        let result = match command.request.method.as_str() {
            "command.status" => {
                client
                    .command_status(&agent_id, &turn_id, p["request_id"].as_str().unwrap_or(""))
                    .await
            }
            "history.list" => {
                let limit = p["limit"].as_u64().unwrap_or(100).clamp(1, 256) as u16;
                client
                    .history(&agent_id, p["before"].as_str(), limit)
                    .await
                    .map(|v| serde_json::to_value(v).unwrap())
            }
            "prompt" => {
                let input = nanocodex_managed::PromptInput::Text(
                    p["input"]["text"].as_str().unwrap_or("").into(),
                );
                client
                    .submit(
                        &agent_id,
                        Some(&command.request.id),
                        &command.request.id,
                        &input,
                    )
                    .await
                    .map(|v| serde_json::to_value(v).unwrap())
            }
            "steer" => {
                let input = nanocodex_managed::PromptInput::Text(
                    p["input"]["text"].as_str().unwrap_or("").into(),
                );
                client
                    .steer_with_id(&agent_id, &turn_id, &command.request.id, &input)
                    .await
                    .map(|v| serde_json::to_value(v).unwrap())
            }
            "cancel" => client
                .cancel_with_id(&agent_id, &turn_id, &command.request.id)
                .await
                .map(|v| serde_json::to_value(v).unwrap()),
            "settings.set" => {
                let settings = &p["settings"];
                let result = if settings.as_object().is_none_or(|v| v.len() != 1) {
                    Err(ManagedError::Configuration(
                        "set exactly one setting".into(),
                    ))
                } else if let Some(model) = settings["model"].as_str() {
                    match model.parse::<Model>() {
                        Ok(model) => client.set_model(&agent_id, model).await,
                        Err(e) => Err(ManagedError::Configuration(e)),
                    }
                } else if let Some(effort) = settings["effort"].as_str() {
                    match effort.parse::<nanocodex::Thinking>() {
                        Ok(effort) => client.set_thinking(&agent_id, effort).await,
                        Err(e) => Err(ManagedError::Configuration(e)),
                    }
                } else {
                    Err(ManagedError::Configuration("unsupported setting".into()))
                };
                result.map(|settings| {
                    updated = Some(settings);
                    serde_json::to_value(settings).unwrap()
                })
            }
            _ => Err(ManagedError::Configuration("unsupported method".into())),
        };
        let result = if matches!(
            command.request.method.as_str(),
            "history.list" | "command.status"
        ) {
            result.unwrap_or_else(unknown)
        } else {
            outcome(result)
        };
        (command, result, updated, agent_id)
    });
}
