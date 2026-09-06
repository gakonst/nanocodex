//! Model-facing task paths and mailbox lifecycle. The embedding's numeric API stays independent.
use super::*;
use nanocodex_agent::{
    AgentHandle, ForkTurns, SpawnOptions,
    input::{AgentMessageContent, Prompt},
};
use std::{collections::VecDeque, sync::Mutex};

pub(crate) struct ModelMessage {
    pub(crate) body: String,
    pub(crate) encrypted: bool,
}

impl ModelMessage {
    fn into_prompt(self, author: String, recipient: String, kind: &str) -> Prompt {
        let mut content = vec![AgentMessageContent::InputText {
            text: format!(
                "Message Type: {kind}\nTask name: {recipient}\nSender: {author}\nPayload:\n"
            )
            .into(),
        }];
        content.push(if self.encrypted {
            AgentMessageContent::EncryptedContent {
                encrypted_content: self.body.into(),
            }
        } else {
            AgentMessageContent::InputText {
                text: self.body.into(),
            }
        });
        Prompt::agent_communication(author, recipient, content)
    }
}

#[derive(Default)]
pub(super) struct Collaboration {
    handles: Mutex<HashMap<String, AgentHandle>>,
    mailboxes: tokio::sync::Mutex<HashMap<String, VecDeque<String>>>,
    spawning: tokio::sync::Mutex<()>,
}

impl AgentScope {
    fn task_path(&self, id: AgentId) -> String {
        let descriptor = &self.sessions[&id].descriptor;
        if descriptor.role.starts_with("/root/") {
            return descriptor.role.clone();
        }
        let parent = descriptor
            .parent
            .map_or_else(|| "/root".into(), |parent| self.task_path(parent));
        format!("{parent}/agent_{id}")
    }
}

impl Registry {
    pub(crate) fn uses_collaboration(&self, session_id: &str) -> bool {
        self.collaboration
            .handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(session_id)
    }

    pub(crate) fn register_handle(&self, handle: AgentHandle) {
        self.collaboration
            .handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(handle.session_id().to_owned(), handle);
    }

    fn message_handle(&self, session_id: &str) -> std::io::Result<AgentHandle> {
        self.collaboration
            .handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .cloned()
            .ok_or_else(|| std::io::Error::other("target agent is not resident"))
    }

    async fn target(
        &self,
        caller: &str,
        target: &str,
    ) -> std::io::Result<(String, Option<AgentId>, String, String)> {
        let state = self.state.lock().await;
        let root = state.root_session_id(caller);
        let scope = state.scopes.get(root);
        let caller_path = scope
            .and_then(|scope| {
                scope
                    .topology
                    .agent_for_session(caller)
                    .map(|id| scope.task_path(id))
            })
            .unwrap_or_else(|| "/root".into());
        if target == "/root" || target == root {
            return Ok((root.to_owned(), None, root.to_owned(), "/root".into()));
        }
        let path = if target.starts_with('/') {
            target.to_owned()
        } else {
            format!("{caller_path}/{target}")
        };
        if let Some(scope) = scope {
            for (&id, session) in &scope.sessions {
                let task_path = scope.task_path(id);
                if task_path == path
                    || session.descriptor.session_id == target
                    || id.to_string() == target
                {
                    if matches!(session.status, AgentStatus::Closing | AgentStatus::Closed) {
                        return Err(std::io::Error::other("target agent is closed"));
                    }
                    return Ok((
                        root.to_owned(),
                        Some(id),
                        session.descriptor.session_id.clone(),
                        task_path,
                    ));
                }
            }
        }
        Err(std::io::Error::other(format!("unknown target {target}")))
    }

    pub(crate) async fn spawn_named(
        self: &Arc<Self>,
        parent: &AgentHandle,
        task_name: String,
        message: ModelMessage,
        options: SpawnOptions,
        fork_turns: ForkTurns,
        host_context: Option<Arc<str>>,
    ) -> std::io::Result<Value> {
        validate_task_name(&task_name)?;
        validate_message(&message.body)?;
        // Serialize named admission, including child materialization, so sibling names cannot race.
        let _spawning = self.collaboration.spawning.lock().await;
        let capacity = self.reserve_turn()?;
        let reservation = self.reserve(parent.session_id()).await?;
        let path = {
            let state = self.state.lock().await;
            let scope = &state.scopes[&reservation.root_session_id];
            let parent_path = reservation
                .parent
                .map_or_else(|| "/root".into(), |id| scope.task_path(id));
            let path = format!("{parent_path}/{task_name}");
            if scope.sessions.keys().any(|id| scope.task_path(*id) == path) {
                return Err(std::io::Error::other("task path is already used"));
            }
            path
        };
        let (child, events) = parent
            .spawn_task(
                options,
                fork_turns,
                Arc::from(path.as_str()),
                host_context.clone(),
            )
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let id = reservation.id;
        let child_session_id = child.session_id().to_owned();
        let descriptor = AgentDescriptor {
            id,
            session_id: child_session_id.clone(),
            role: path.clone(),
            task: if message.encrypted {
                "[Encrypted delegated task]".into()
            } else {
                message.body.clone()
            },
            parent: reservation.parent,
        };
        let (start_events, events_ready) = oneshot::channel();
        let event_task = forward_events(
            reservation.root_session_id.clone(),
            id,
            events,
            events_ready,
            Arc::downgrade(self),
            self.updates.clone(),
        );
        let mut startup = self.batch_startup();
        self.insert(
            reservation.root_session_id.clone(),
            descriptor.clone(),
            host_context,
            child,
            event_task,
            OutputContract::plain(),
        )
        .await?;
        startup.track(&reservation.root_session_id, id);
        self.send(&reservation.root_session_id, AgentUpdate::Added(descriptor));
        let _ = start_events.send(());
        let prompt = message.into_prompt(
            self.path_for_session(parent.session_id()).await,
            path.clone(),
            "NEW_TASK",
        );
        self.launch_initial_turn(&reservation.root_session_id, id, prompt, capacity)
            .await?;
        startup.commit();
        Ok(serde_json::json!({"task_name": path}))
    }

    pub(crate) async fn collaboration_message(
        &self,
        caller: &str,
        target: &str,
        message: ModelMessage,
        followup: bool,
    ) -> std::io::Result<()> {
        validate_message(&message.body)?;
        let (root, id, receiver, path) = self.target(caller, target).await?;
        if followup && id.is_none() {
            return Err(std::io::Error::other(
                "follow-up tasks cannot target the root agent",
            ));
        }
        let sender = self.path_for_session(caller).await;
        let prompt = message.into_prompt(
            sender.clone(),
            path,
            if followup { "NEW_TASK" } else { "MESSAGE" },
        );
        if followup {
            let harness = self
                .state
                .lock()
                .await
                .harness_in_scope(&root, id.expect("non-root target"))?;
            harness.followup(prompt).await?;
        } else {
            self.message_handle(&receiver)?
                .queue_message(prompt)
                .await
                .map_err(|error| std::io::Error::other(error.to_string()))?;
        }
        self.record_notice(&receiver, sender).await;
        Ok(())
    }

    async fn path_for_session(&self, session_id: &str) -> String {
        let state = self.state.lock().await;
        state
            .scopes
            .get(state.root_session_id(session_id))
            .and_then(|scope| {
                scope
                    .topology
                    .agent_for_session(session_id)
                    .map(|id| scope.task_path(id))
            })
            .unwrap_or_else(|| "/root".into())
    }

    async fn record_notice(&self, receiver: &str, sender: String) {
        let mut mailboxes = self.collaboration.mailboxes.lock().await;
        let mailbox = mailboxes.entry(receiver.to_owned()).or_default();
        if !mailbox.contains(&sender) {
            mailbox.push_back(sender);
        }
        drop(mailboxes);
        self.changed();
    }

    pub(super) async fn notify_parent(&self, root: &str, id: AgentId, status: &AgentStatus) {
        let target = {
            let state = self.state.lock().await;
            let Some(scope) = state.scopes.get(root) else {
                return;
            };
            let Some(session) = scope.sessions.get(&id) else {
                return;
            };
            if !session.plain_result {
                return;
            }
            let parent = session
                .descriptor
                .parent
                .and_then(|parent| scope.sessions.get(&parent));
            (
                parent
                    .map_or(root, |parent| parent.descriptor.session_id.as_str())
                    .to_owned(),
                scope.task_path(id),
            )
        };
        let (parent, path) = target;
        let output = match status {
            AgentStatus::Completed {
                output: Value::String(output),
            } => output.clone(),
            other => serde_json::to_string(other).unwrap_or_default(),
        };
        let prompt = ModelMessage {
            body: output,
            encrypted: false,
        }
        .into_prompt(
            path.clone(),
            self.path_for_session(&parent).await,
            "FINAL_ANSWER",
        );
        if let Ok(handle) = self.message_handle(&parent) {
            // Admission follows the agent driver's durable checkpoint / steering rules.
            if handle.queue_message(prompt).await.is_ok() {
                self.record_notice(&parent, path).await;
            }
        }
    }

    pub(crate) async fn collaboration_directory(
        &self,
        caller: &str,
        prefix: Option<&str>,
    ) -> std::io::Result<Value> {
        let caller_path = self.path_for_session(caller).await;
        let prefix = prefix
            .map(|prefix| resolve_path(prefix, &caller_path))
            .transpose()?;
        let prefix = prefix.as_deref();
        let state = self.state.lock().await;
        let root = state.root_session_id(caller);
        let mut agents = Vec::new();
        if prefix.is_none_or(|prefix| "/root" == prefix)
            && let Ok(handle) = self.message_handle(root)
        {
            agents.push(serde_json::json!({"agent_name":"/root","agent_status":handle.collaboration_status()}));
        }
        if let Some(scope) = state.scopes.get(root) {
            let mut entries = scope.sessions.iter().collect::<Vec<_>>();
            entries.sort_unstable_by_key(|(id, _)| scope.task_path(**id));
            for (&id, session) in entries {
                if matches!(session.status, AgentStatus::Closed) {
                    continue;
                }
                let path = scope.task_path(id);
                if prefix.is_some_and(|prefix| {
                    path != prefix && !path.starts_with(&format!("{prefix}/"))
                }) {
                    continue;
                }
                agents.push(serde_json::json!({"agent_name":path,"agent_status":model_status(&session.status)}));
            }
        }
        Ok(serde_json::json!({"agents":agents}))
    }

    pub(crate) async fn wait_mailbox(&self, caller: &str, duration: Duration) -> Value {
        let mut revision = self.revision.subscribe();
        let caller_handle = self.message_handle(caller).ok();
        let deadline = Instant::now() + duration;
        loop {
            let updates = self
                .collaboration
                .mailboxes
                .lock()
                .await
                .remove(caller)
                .unwrap_or_default();
            if !updates.is_empty() {
                return serde_json::json!({"message":"Wait completed.","timed_out":false});
            }
            tokio::select! {
                outcome = timeout_at(deadline, revision.changed()) => {
                    if outcome.is_err() { return serde_json::json!({"message":"Wait timed out.","timed_out":true}); }
                }
                received = async {
                    match &caller_handle { Some(handle) => handle.wait_for_user_input().await,
                        None => std::future::pending().await }
                } => {
                    return serde_json::json!({"message":if received { "Wait interrupted by new input." } else { "Agent stopped." },"timed_out":false});
                }
            }
        }
    }

    pub(crate) async fn interrupt_target(
        &self,
        caller: &str,
        target: &str,
    ) -> std::io::Result<Value> {
        let (root, id, receiver, _) = self.target(caller, target).await?;
        let id = id.ok_or_else(|| std::io::Error::other("cannot interrupt the root agent"))?;
        if receiver == caller {
            return Err(std::io::Error::other("cannot interrupt yourself"));
        }
        let (harness, previous) = {
            let state = self.state.lock().await;
            (
                state.harness_in_scope(&root, id)?,
                state.scopes[&root].sessions[&id].status.clone(),
            )
        };
        timeout_at(Instant::now() + AGENT_STOP_TIMEOUT, harness.interrupt())
            .await
            .map_err(|_| std::io::Error::other("agent interruption timed out"))??;
        Ok(serde_json::json!({"previous_status":model_status(&previous)}))
    }
}

fn model_status(status: &AgentStatus) -> Value {
    match status {
        AgentStatus::Pending => Value::String("pending_init".into()),
        AgentStatus::Running => Value::String("running".into()),
        AgentStatus::Interrupted => Value::String("interrupted".into()),
        AgentStatus::Closing | AgentStatus::Closed => Value::String("shutdown".into()),
        AgentStatus::Failed { error } => serde_json::json!({"errored":error}),
        AgentStatus::Completed { output } => {
            serde_json::json!({"completed":output.as_str().map(str::to_owned).unwrap_or_else(|| output.to_string())})
        }
    }
}

fn validate_task_name(name: &str) -> std::io::Result<()> {
    if name.is_empty()
        || name == "root"
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(std::io::Error::other(
            "task_name must use lowercase letters, digits, and underscores; root is reserved",
        ));
    }
    Ok(())
}

fn resolve_path(reference: &str, caller: &str) -> std::io::Result<String> {
    if reference == "/root" {
        return Ok(reference.to_owned());
    }
    let segments = if reference.starts_with('/') {
        reference
            .strip_prefix("/root/")
            .ok_or_else(|| std::io::Error::other("absolute task paths must start with /root"))?
    } else {
        reference
    };
    for segment in segments.split('/') {
        validate_task_name(segment)?;
    }
    Ok(if reference.starts_with('/') || caller.is_empty() {
        reference.to_owned()
    } else {
        format!("{caller}/{reference}")
    })
}

fn validate_message(message: &str) -> std::io::Result<()> {
    if message.trim().is_empty() {
        Err(std::io::Error::other("message must not be empty"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_paths_match_upstream_validation() {
        for invalid in ["", "root", "../sibling", "Upper", "a/b"] {
            assert!(validate_task_name(invalid).is_err());
        }
        assert!(validate_task_name(&"a".repeat(128)).is_ok());
        for invalid in ["/rootish", "/root/", "/root/a/root", "a/", ""] {
            assert!(resolve_path(invalid, "/root/a").is_err());
        }
        assert_eq!(
            resolve_path("child/check", "/root/a").unwrap(),
            "/root/a/child/check"
        );
        assert_eq!(resolve_path("/root/b", "/root/a").unwrap(), "/root/b");
    }

    #[tokio::test]
    async fn canonical_paths_are_relative_to_the_caller_and_fenced_by_root() {
        let (registry, _, _) = crate::channel(4);
        let descriptor = |id, session: &str, path: &str, parent| AgentDescriptor {
            id: AgentId::new(id),
            session_id: session.into(),
            role: path.into(),
            task: "task".into(),
            parent,
        };
        registry
            .restore(
                "first",
                vec![
                    descriptor(1, "a", "/root/a", None),
                    descriptor(2, "a_child", "/root/a/check", Some(AgentId::new(1))),
                    descriptor(3, "b", "/root/b", None),
                    descriptor(4, "b_child", "/root/b/check", Some(AgentId::new(3))),
                ],
            )
            .await
            .unwrap();
        registry
            .restore("second", vec![descriptor(1, "foreign", "/root/a", None)])
            .await
            .unwrap();
        assert_eq!(registry.target("a", "check").await.unwrap().2, "a_child");
        assert_eq!(registry.target("b", "check").await.unwrap().2, "b_child");
        assert_eq!(
            registry.target("b", "/root/a/check").await.unwrap().2,
            "a_child"
        );
        assert_eq!(registry.target("a", "/root").await.unwrap().2, "first");
        assert!(registry.target("a", "foreign").await.is_err());
        assert!(registry.target("first", "check").await.is_err());
        assert_eq!(registry.target("second", "a").await.unwrap().2, "foreign");
        let listed = registry
            .collaboration_directory("a", Some("/root/a"))
            .await
            .unwrap();
        assert_eq!(listed["agents"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn mailbox_wait_consumes_each_senders_notice_and_isolates_recipients() {
        let (registry, _, _) = crate::channel(4);
        registry.record_notice("first", "/root/a".into()).await;
        registry.record_notice("first", "/root/a".into()).await;
        registry.record_notice("second", "/root/b".into()).await;
        let first = registry.wait_mailbox("first", Duration::ZERO).await;
        assert_eq!(first["timed_out"], false);
        assert_eq!(
            registry.wait_mailbox("first", Duration::ZERO).await["timed_out"],
            true
        );
        assert_eq!(
            registry.wait_mailbox("second", Duration::ZERO).await["timed_out"],
            false
        );
    }
}
