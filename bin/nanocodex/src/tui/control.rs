use super::*;
use nanocodex::agent::session::Origin;
use nanocodex_tui_control::{Command, Conversation, accepted, rejected, unknown};
use serde_json::{Value, json};

pub(super) fn dispatch(
    ui: &mut UiModel,
    command: Command,
    tx: &mpsc::UnboundedSender<WorkerCommand>,
) -> Result<()> {
    let bridge = ui.control.as_ref().unwrap();
    let method = command.request.method.as_str();
    if matches!(method, "models.list" | "history.list" | "history.read") {
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
    if method == "command" {
        let result = run_command(ui, &command.request.params, tx);
        command.finish(result);
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

/// Runs one slash command as if submitted from the composer, leaving the
/// user's draft untouched. Plain text belongs to `prompt`; menus that need
/// terminal interaction are rejected. Acceptance means the command was
/// dispatched, not that its effects have completed.
fn run_command(
    ui: &mut UiModel,
    params: &Value,
    tx: &mpsc::UnboundedSender<WorkerCommand>,
) -> Value {
    let text = params["input"]["text"].as_str().unwrap_or("").trim();
    if !text.starts_with('/') || text.contains('\n') {
        return rejected("not_a_command");
    }
    let submission = classify_submission(SubmittedPrompt::text(text.to_owned()));
    match submission {
        Submission::Prompt(ref prompt) if !prompt.has_instruction() => rejected("not_a_command"),
        Submission::Prompt(_) if ui.app.control_snapshot()["execution"] != "idle" => {
            rejected("busy")
        }
        Submission::ModelPicker | Submission::ReasoningPicker => rejected("interactive_command"),
        Submission::InvalidCommand(message) => {
            json!({"status":"rejected","code":"invalid_command","message":message})
        }
        submission => {
            let root = std::sync::Arc::clone(&ui.root_session_id);
            match execute_submission(&mut ui.app, &root, tx, SubmitIntent::Queue, submission) {
                Ok(()) => accepted(json!({})),
                Err(error) => unknown(error),
            }
        }
    }
}

/// Rollout mirror of one session, when it records one.
pub(super) fn rollout(agent: &Nanocodex) -> Option<nanocodex::agent::rollout::RolloutInfo> {
    agent
        .persistence()
        .and_then(|persistence| persistence.rollout)
}

/// Whether another process can resume this session, for /split and /collapse.
pub(super) fn resumable(agent: &Nanocodex) -> bool {
    agent
        .persistence()
        .is_some_and(|persistence| persistence.resumable())
}

fn descriptor(agent: &Nanocodex) -> Conversation {
    let session = agent.session();
    let lineage = &session.lineage;
    let (origin, role) = match lineage.origin {
        Origin::Root => ("root", "root"),
        Origin::Fork | Origin::Branch => ("fork", "branch"),
        Origin::SideConversation => ("fork", "side_conversation"),
        Origin::Subagent => ("spawn", "subagent"),
        // Later provenance kinds are branches of the same conversation tree.
        _ => ("fork", "branch"),
    };
    Conversation {
        session_id: session.session_id.clone(),
        root_session_id: Some(lineage.root_session_id.clone()),
        parent_session_id: lineage.parent_session_id.clone(),
        origin: origin.into(),
        role: role.into(),
        rollout_path: rollout(agent).map(|rollout| rollout.path().to_path_buf()),
    }
}

pub(super) fn models() -> Value {
    let efforts = Thinking::ALL;
    json!({"models":HarnessModel::for_family(nanocodex::HarnessFamily::Codex).chain(HarnessModel::for_family(nanocodex::HarnessFamily::Claude)).map(|model|
        json!({"id":model.as_str(),"efforts":efforts.iter().filter(|e| model.supports_thinking(**e)).map(ToString::to_string).collect::<Vec<_>>()})).collect::<Vec<_>>()})
}

impl AgentWorker {
    pub(super) fn publish_control_conversations(&self) {
        let Some(bridge) = &self.control else {
            return;
        };
        let agents = std::iter::once(&self.main)
            .chain(&self.archived_main)
            .map(|branch| &branch.agent)
            .chain(self.btw.iter().map(|btw| &btw.agent));
        for agent in agents {
            if let Some(rollout) = rollout(agent) {
                bridge.committed(agent.session_id(), rollout.committed_bytes());
            }
            bridge.conversation(descriptor(agent));
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
        if self.model_selection_required
            && matches!(command.request.method.as_str(), "prompt" | "steer")
        {
            command.finish(json!({"status":"rejected","code":"model_selection_required","message":MODEL_SELECTION_REQUIRED}));
            return;
        }
        let expected = command.request.params["expected_session_id"]
            .as_str()
            .unwrap_or("")
            .to_owned();
        if matches!(
            command.request.method.as_str(),
            "history.list" | "history.read"
        ) {
            // Reads target known conversations independently of the composer's focus.
            let agent = std::iter::once(&self.main.agent)
                .chain(self.archived_main.iter().map(|b| &b.agent))
                .chain(self.btw.iter().map(|b| &b.agent))
                .find(|a| a.session_id() == expected);
            let Some(rollout) = agent.and_then(rollout) else {
                command.reject("history_unavailable");
                return;
            };
            let path = rollout.path().to_path_buf();
            let boundary = rollout.committed_bytes();
            tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    let read = || -> std::io::Result<Value> {
                        let file = std::fs::File::open(path)?;
                        if command.request.method == "history.read" {
                            nanocodex_tui_control::history::chunk(file, boundary, &command.request.params)
                        } else { nanocodex_tui_control::history::page(file, boundary, &command.request.params) }
                    };
                    let value = read().unwrap_or_else(|error| json!({"status":"rejected","code":"history_unavailable","message":error.to_string()}));
                    command.finish(value);
                }).await;
                let _ = result;
            });
            return;
        }
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
                    // The bridge ledger owns request deduplication and the receipt carries
                    // the canonical turn ID. A prompt request ID would require an execution
                    // policy, which the native TUI does not configure.
                    if self
                        .prompt_identified(
                            target,
                            input_id.unwrap(),
                            SubmittedPrompt::text(text),
                            None,
                        )
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
                    match model.parse::<HarnessModel>() {
                        Ok(model) => self.change_model(model).await,
                        Err(e) => Err(e.to_owned()),
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
