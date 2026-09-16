use super::*;
use nanocodex_tui_control::{Command, Conversation, accepted, rejected, unknown};
use serde_json::{Value, json};

pub(super) fn dispatch(
    ui: &mut UiModel,
    command: Command,
    tx: &mpsc::UnboundedSender<WorkerCommand>,
) -> Result<()> {
    let bridge = ui.control.as_ref().unwrap();
    let method = command.request.method.as_str();
    if matches!(method, "models.list" | "history.list") {
        tx.send(WorkerCommand::Control {
            command,
            target: ui.app.focus,
            input_id: None,
        })?;
        return Ok(());
    }
    if let Err(code) = bridge.validate(&command.request) {
        command.reject(code);
        return Ok(());
    }
    if method == "prompt" && ui.app.control_snapshot()["execution"] != "idle" {
        command.reject("busy");
        return Ok(());
    }
    let input = command.request.params["input"]["text"]
        .as_str()
        .unwrap_or("");
    if matches!(method, "prompt" | "steer") && input.trim().is_empty() {
        command.reject("empty_input");
        return Ok(());
    }
    // Reserve display identity without taking or modifying the user's draft.
    let input_id = match method {
        "prompt" => ui.app.queue_prompt(ui.app.focus, input.to_owned()),
        "steer" => ui
            .app
            .queue_steer(ui.app.focus, SubmittedPrompt::text(input.to_owned())),
        _ => None,
    };
    if matches!(method, "prompt" | "steer") && input_id.is_none() {
        command.reject("composer_unavailable");
        return Ok(());
    }
    tx.send(WorkerCommand::Control {
        command,
        target: ui.app.focus,
        input_id,
    })?;
    Ok(())
}

fn descriptor(agent: &Nanocodex, root: &str, parent: Option<&str>, role: &str) -> Conversation {
    let metadata = agent
        .rollout()
        .and_then(|rollout| {
            use std::io::BufRead;
            let file = std::fs::File::open(rollout.path()).ok()?;
            let line = std::io::BufReader::new(file).lines().next()?.ok()?;
            serde_json::from_str::<Value>(&line)
                .ok()
                .map(|v| v["payload"].clone())
        })
        .unwrap_or_default();
    let root = metadata["root_session_id"].as_str().unwrap_or(root);
    let parent = metadata["parent_thread_id"].as_str().or(parent);
    let role = metadata["conversation_role"].as_str().unwrap_or(role);
    Conversation {
        session_id: agent.session_id().into(),
        root_session_id: Some(root.into()),
        parent_session_id: parent.map(str::to_owned),
        origin: match role {
            "branch" | "side_conversation" => "fork",
            "subagent" => "spawn",
            _ => "root",
        }
        .into(),
        role: role.into(),
        rollout_path: agent.rollout().map(|r| r.path().to_path_buf()),
    }
}

pub(super) fn models() -> Value {
    let efforts = Thinking::ALL;
    json!({"models":Model::ALL.into_iter().map(|model|
        json!({"id":model.as_str(),"efforts":efforts.iter().filter(|e| model.supports_thinking(**e)).map(ToString::to_string).collect::<Vec<_>>()})).collect::<Vec<_>>()})
}

impl AgentWorker {
    pub(super) fn publish_control_conversations(&self) {
        let Some(bridge) = &self.control else {
            return;
        };
        let root = self
            .archived_main
            .iter()
            .find(|b| b.id == 0)
            .unwrap_or(&self.main)
            .agent
            .session_id();
        for branch in std::iter::once(&self.main).chain(&self.archived_main) {
            bridge.conversation(descriptor(
                &branch.agent,
                root,
                None,
                if branch.id == 0 { "root" } else { "branch" },
            ));
        }
        if let Some(btw) = &self.btw {
            bridge.conversation(descriptor(
                &btw.agent,
                root,
                Some(self.main.agent.session_id()),
                "side_conversation",
            ));
        }
    }

    pub(super) async fn control_command(
        &mut self,
        command: Command,
        target: PaneId,
        input_id: Option<u64>,
    ) {
        let Some(bridge) = self.control.clone() else {
            command.reject("unavailable");
            return;
        };
        if command.request.method == "models.list" {
            command.finish(models());
            return;
        }
        let expected = command.request.params["expected_session_id"]
            .as_str()
            .unwrap_or("")
            .to_owned();
        let owner = match target {
            PaneId::Main => Some((&self.main.agent, &self.main.turns)),
            PaneId::Btw(id) => self
                .btw
                .as_ref()
                .filter(|b| b.id == id)
                .map(|b| (&b.agent, &b.turns)),
        };
        let Some((agent, turns)) = owner else {
            command.reject("session_changed");
            return;
        };
        if expected != agent.session_id() {
            command.reject("session_changed");
            return;
        }
        let agent = agent.clone();
        if command.request.method == "history.list" {
            let path = agent.rollout().map(|r| r.path().to_path_buf());
            let offset = command.request.params["after_line"].as_u64().unwrap_or(0) as usize;
            let limit = command.request.params["limit"]
                .as_u64()
                .unwrap_or(100)
                .clamp(1, 256) as usize;
            tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(move || -> std::io::Result<Value> {
                    use std::io::BufRead;
                    let path = path.ok_or_else(|| std::io::Error::other("rollout disabled"))?;
                    let file = std::io::BufReader::new(std::fs::File::open(path)?);
                    let mut records = Vec::new();
                    let mut bytes = 0;
                    for line in file.lines().skip(offset).take(limit) {
                        let line = line?;
                        bytes += line.len();
                        if bytes > nanocodex_tui_control::MAX_FRAME / 2 {
                            if records.is_empty() {
                                return Ok(rejected("history_record_too_large"));
                            }
                            break;
                        }
                        if let Ok(value) = serde_json::from_str::<Value>(&line) {
                            records.push(value);
                        } else {
                            break;
                        }
                    }
                    Ok(json!({"next_line":offset + records.len(),"records":records}))
                })
                .await;
                command.finish(match result {
                    Ok(Ok(v)) => v,
                    _ => rejected("history_unavailable"),
                });
            });
            return;
        }
        if let Err(code) = bridge.validate(&command.request) {
            command.reject(code);
            return;
        }
        let turn_id = command.request.params["expected_turn_id"]
            .as_str()
            .unwrap_or("");
        let selected = turns
            .iter()
            .find(|t| t.canonical_id == turn_id)
            .map(|t| t.control.clone());
        let method = command.request.method.clone();
        let result = match method.as_str() {
            "prompt" => {
                if !turns.is_empty() {
                    rejected("busy")
                } else {
                    let text = command.request.params["input"]["text"]
                        .as_str()
                        .unwrap_or("")
                        .to_owned();
                    if self
                        .prompt(target, input_id.unwrap(), SubmittedPrompt::text(text))
                        .await
                    {
                        let turns = match target {
                            PaneId::Main => &self.main.turns,
                            PaneId::Btw(_) => &self.btw.as_ref().unwrap().turns,
                        };
                        accepted(json!({"turn_id":turns.back().unwrap().canonical_id}))
                    } else {
                        rejected("admission_failed")
                    }
                }
            }
            "steer" => {
                if let Some(turn) = selected {
                    let input = command.request.params["input"]["text"]
                        .as_str()
                        .unwrap_or("");
                    match turn.steer_with_id(command.request.id.clone(), input).await {
                        Ok(()) => {
                            let _ = self.updates.send(WorkerEvent::SteerAdmitted {
                                target,
                                id: input_id.unwrap(),
                            });
                            accepted(json!({"turn_id":turn_id}))
                        }
                        Err(NanocodexError::TurnNotSteerable) => rejected("turn_not_active"),
                        Err(error) => unknown(error),
                    }
                } else {
                    rejected("turn_not_active")
                }
            }
            "cancel" => {
                if let Some(turn) = selected {
                    match turn.cancel().await {
                        Ok(()) => accepted(json!({"turn_id":turn_id})),
                        Err(e) => unknown(e),
                    }
                } else {
                    rejected("turn_not_active")
                }
            }
            "settings.set" => {
                if target != PaneId::Main {
                    command.reject("settings_require_main_conversation");
                    return;
                }
                let settings = &command.request.params["settings"];
                let outcome = if settings.as_object().is_none_or(|v| v.len() != 1) {
                    Err("set exactly one setting".to_owned())
                } else if let Some(model) = settings["model"].as_str() {
                    match model.parse::<Model>() {
                        Ok(model) => agent
                            .set_model(model)
                            .await
                            .map(|()| {
                                let _ = self.updates.send(WorkerEvent::ModelChanged { model });
                            })
                            .map_err(|e| e.to_string()),
                        Err(e) => Err(e),
                    }
                } else if let Some(effort) = settings["effort"].as_str() {
                    match effort.parse::<Thinking>() {
                        Ok(thinking) => agent
                            .set_thinking(thinking)
                            .await
                            .map(|()| {
                                let _ =
                                    self.updates.send(WorkerEvent::ThinkingChanged { thinking });
                            })
                            .map_err(|e| e.to_string()),
                        Err(e) => Err(e),
                    }
                } else {
                    Err("unsupported setting".into())
                };
                match outcome {
                    Ok(()) => {
                        bridge.settings_committed(settings.clone());
                        accepted(json!({"settings":settings}))
                    }
                    Err(e) => json!({"status":"rejected","code":"invalid_settings","message":e}),
                }
            }
            _ => rejected("unsupported_method"),
        };
        command.finish(result);
    }
}
