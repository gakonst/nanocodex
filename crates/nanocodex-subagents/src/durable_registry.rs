#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
enum ChildCall {
    Spawn { input: Value, id: AgentId },
    Message { input: Value, receipt: MessageReceipt },
    Submit { input: Value, receipt: Value },
}

// Durable registry transitions run under the same registry lock as their live
// counterpart. A failed store write poisons the journal, preventing another
// receipt or inference admission until authoritative cold reconstruction.
impl Registry {
    /// Declares that this registry must acquire its durable tree before tools run.
    /// Hosted factories call this before installing native Claude callbacks.
    pub fn require_durability(&self) {
        self.durable_required.store(true, Ordering::Release);
    }

    pub(super) fn durable_replay(&self) -> bool {
        self.durable_required.load(Ordering::Acquire)
    }

    pub(super) async fn replay_spawn(&self, session: &str, key: &str, input: &Value) -> std::io::Result<Option<super::tools::AgentStartReport>> {
        let mut state = self.state.lock().await;
        let root = state.root_session_id(session).to_owned();
        let scope = state.scope_mut(&root);
        if self.durable_replay() && scope.journal.is_none() { return Err(std::io::Error::other("durable child journal is not ready")); }
        // A poisoned or superseded journal must never issue a replay receipt.
        scope.persist().await?;
        match scope.calls.get(key) {
            Some(ChildCall::Spawn { input: previous, id }) if previous == input => {
                let child = &scope.sessions[id];
                Ok(Some(super::tools::AgentStartReport { agent_id: *id, role: child.descriptor.role.clone(), status: AgentStatus::Running }))
            }
            Some(_) => Err(std::io::Error::other("child tool call identity reused with different arguments")),
            None => Ok(None),
        }
    }

    pub(super) async fn submit_result_keyed(&self, session: &str, revision: Option<u64>, output: Value, key: String, input: Value) -> std::io::Result<Value> {
        let mut state = self.state.lock().await;
        let root = state.root_session_id(session).to_owned();
        if self.durable_replay() && state.scope_mut(&root).journal.is_none() { return Err(std::io::Error::other("durable child journal is not ready")); }
        if let Some(call) = state.scope_mut(&root).calls.get(&key).cloned() {
            state.scope_mut(&root).persist().await?;
            return match call { ChildCall::Submit { input: old, receipt } if old == input => Ok(receipt), _ => Err(std::io::Error::other("child tool call identity reused with different arguments")) };
        }
        let outcome = state.submit_result(session, revision, output)?;
        let receipt = match outcome {
            SubmissionOutcome::Accepted { decoded_json_text } => {
                let mut receipt = serde_json::json!({ "accepted": true, "status": "accepted" });
                if decoded_json_text { receipt["decoded_json_text"] = true.into(); }
                receipt
            }
            SubmissionOutcome::Superseded => serde_json::json!({ "accepted": false, "status": "superseded" }),
        };
        let scope = state.scope_mut(&root);
        if scope.journal.is_some() { scope.calls.insert(key, ChildCall::Submit { input, receipt: receipt.clone() }); }
        scope.persist().await?;
        Ok(receipt)
    }

    /// Reopens one child tree on the host's existing fenced durability store.
    /// Configure a per-child durable native factory before calling `recover`.
    pub async fn enable_durability(
        &self,
        #[cfg(not(target_family = "wasm"))] store: impl StateStore + 'static,
        #[cfg(target_family = "wasm")] store: impl StateStore + Send + 'static,
        root_session_id: &str,
    ) -> std::io::Result<()> {
        self.require_durability();
        let mut state = self.state.lock().await;
        if state.scopes.get(root_session_id).is_some_and(|scope| scope.journal.is_some() || !scope.sessions.is_empty()) {
            return Err(std::io::Error::other("child durability must be enabled before spawning or recovery"));
        }
        let mut journal = ChildJournal::open(store, root_session_id).await.map_err(std::io::Error::other)?;
        let mut scope = AgentScope::default();
        if let Some(record) = journal.load::<ChildTreeRecord>().await.map_err(std::io::Error::other)? {
            if record.version != 1 { return Err(std::io::Error::other("unsupported child tree journal version")); }
            scope.calls = record.calls;
            scope.steer_intents = record.steer_intents;
            scope.topology = record.topology;
            scope.messages = record.messages;
            scope.closing = record.closing;
            for (id, record) in record.sessions {
                let contract = OutputContract::compile(&record.output_schema)?;
                if id != record.descriptor.id || scope.topology.agent_for_session(&record.descriptor.session_id) != Some(id) {
                    return Err(std::io::Error::other("invalid child topology checkpoint"));
                }
                state.root_by_session.insert(record.descriptor.session_id.clone(), root_session_id.to_owned());
                let status = if matches!(record.status, AgentStatus::Running) { AgentStatus::Interrupted } else { record.status };
                scope.sessions.insert(id, ChildSession {
                    descriptor: record.descriptor, host_context: record.host_context.map(Arc::from),
                    event_task: None, harness: None, harness_task: None,
                    status, active: false, output_validator: contract.validator,
                    output_schema: record.output_schema, stored_runtime: record.snapshot,
                    next_instruction_revision: record.next_instruction_revision,
                    active_instruction_revision: record.active_instruction_revision,
                    steering: false, submitted_output: record.submitted_output, last_output: record.last_output,
                    last_used: 0, evicted: true, execution: record.execution, interrupted: record.interrupted,
                });
            }
        }
        scope.journal = Some(journal);
        state.scopes.insert(root_session_id.to_owned(), scope);
        Ok(())
    }

    /// Reopens a configured root before inference; child hooks share its tree.
    pub async fn recover_registered(self: &Arc<Self>, session: &str) -> std::io::Result<()> {
        let empty = {
            let state = self.state.lock().await;
            if state.root_by_session.contains_key(session)
                || !state.scopes.get(session).is_some_and(|scope| scope.journal.is_some()) {
                return Ok(());
            }
            state.scopes[session].sessions.is_empty()
        };
        let handle = self.session_handles.read().expect("session handles poisoned")
            .get(session).cloned();
        // Retirement can leave an already-owned root completion to settle. An
        // empty tree has no child work to reconstruct; a live owner still checks
        // the journal so startup failures remain observable through ready().
        let Some(handle) = handle else {
            return if empty { Ok(()) } else { Err(std::io::Error::other("parent handle unavailable for child recovery")) };
        };
        self.recover(handle).await
    }

    /// Reconnects stable child capabilities and resumes unfinished work.
    /// Committed child effects replay in each child's own `DurableSession`.
    /// Repeated calls leave resident drivers and their active turns untouched.
    pub async fn recover(self: &Arc<Self>, parent: AgentHandle) -> std::io::Result<()> {
        let root = parent.session_id().to_owned();
        self.register_handle(parent);
        let _residency = self.residency_lock.lock().await;
        let _messages = self.message_lock.lock().await;
        let (ids, closing) = {
            let mut state = self.state.lock().await;
            let Some(scope) = state.scopes.get_mut(&root) else { return Ok(()); };
            // Verifies this generation before opening child execution owners.
            scope.persist().await?;
            (scope.topology.all_postorder().into_iter().rev().collect::<Vec<_>>(), scope.closing)
        };
        for id in ids {
            let (descriptor, status, resident, execution, interrupted) = {
                let state = self.state.lock().await;
                let child = &state.scopes[&root].sessions[&id];
                (child.descriptor.clone(), child.status.clone(), child.harness.is_some(), child.execution.clone(), child.interrupted)
            };
            self.send(&root, AgentUpdate::Added(descriptor.clone()));
            self.send(&root, AgentUpdate::Status { id, status: status.clone() });
            if resident || matches!(status, AgentStatus::Closed) { continue; }
            if closing || matches!(status, AgentStatus::Closing) {
                // Closing was committed before any cancellation request. Restore
                // the native journal so cancellation cannot resurrect on restart.
                self.rehydrate_for_close(&root, id).await?;
                continue;
            }
            self.rehydrate(&root, id, MessagePurpose::Coordinate).await?;
            let harness = self.state.lock().await.harness_in_scope(&root, id)?;
            if let Some(execution) = execution {
                let capacity = self.reserve_turn()?;
                harness.start(execution.prompt, capacity).await?;
                if interrupted { harness.interrupt().await?; }
            } else if matches!(status, AgentStatus::Pending) {
                harness.start(super::model::agent_prompt(id, &descriptor.task), self.reserve_turn()?).await?;
            }
            let pending = self.state.lock().await.scopes[&root].messages.pending_for(id);
            for message in pending {
                harness.enqueue_delivery(message)?.release().await?;
            }
        }
        Ok(())
    }

    async fn rehydrate_for_close(self: &Arc<Self>, root: &str, id: AgentId) -> std::io::Result<()> {
        // Temporarily permit construction, without publishing a reusable status.
        {
            let mut state = self.state.lock().await;
            state.scopes.get_mut(root).expect("scope").sessions.get_mut(&id).expect("child").status = AgentStatus::Interrupted;
        }
        self.rehydrate(root, id, MessagePurpose::Coordinate).await?;
        let harness = {
            let mut state = self.state.lock().await;
            let child = state.scopes.get_mut(root).expect("scope").sessions.get_mut(&id).expect("child");
            child.status = AgentStatus::Closing;
            child.harness.clone().ok_or_else(|| std::io::Error::other("closing child checkpoint missing"))?
        };
        // Shutdown of the native driver cancels its own retained operation.
        harness.close().await?;
        Ok(())
    }

    /// Background admission requires a fenced tree owned by this parent.
    pub async fn validate_lifetime(&self, session: &str, lifetime: AgentLifetime) -> std::io::Result<()> {
        if lifetime == AgentLifetime::Foreground { return Ok(()); }
        let mut state = self.state.lock().await;
        let root = state.root_session_id(session).to_owned();
        let scope = state.scope_mut(&root);
        if scope.journal.is_none() {
            return Err(std::io::Error::other("background children require a durable parent"));
        }
        scope.persist().await
    }

    /// Whether the background tree has unfinished execution or queued delivery.
    /// Terminal reusable children retain identity without retaining an alarm.
    pub async fn has_background(&self, session: &str) -> bool {
        let state = self.state.lock().await;
        let root = state.root_session_id(session);
        state.scopes.get(root).is_some_and(|scope| {
            scope.sessions.iter().any(|(&id, child)| {
                !matches!(child.status, AgentStatus::Closing | AgentStatus::Closed)
                    && ((!child.interrupted && (child.execution.is_some()
                        || matches!(child.status, AgentStatus::Pending | AgentStatus::Running)))
                        || scope.messages.has_pending_for(id))
                    && scope.sessions.iter().any(|(&bg, ancestor)| {
                        ancestor.descriptor.lifetime == AgentLifetime::Background
                            && !matches!(ancestor.status, AgentStatus::Closing | AgentStatus::Closed)
                            && (id == bg || scope.topology.is_descendant(id, bg))
                    })
            })
        })
    }

    pub async fn summaries_all(&self, session: &str) -> std::io::Result<Vec<AgentSummary>> {
        let mut state = self.state.lock().await;
        let root = state.root_session_id(session).to_owned();
        let Some(scope) = state.scopes.get_mut(&root) else { return Ok(Vec::new()); };
        scope.persist().await?;
        let mut ids = scope.topology.ids();
        ids.sort_unstable();
        state.summaries(session, &ids)
    }

    /// Holds parent completion until its owned foreground subtree is idle.
    /// A background boundary detaches that subtree from the caller's lifetime.
    pub async fn wait_foreground(&self, session: &str) -> std::io::Result<()> {
        let mut revision = self.revision.subscribe();
        loop {
            let busy = {
                let mut state = self.state.lock().await;
                let root = state.root_session_id(session).to_owned();
                let Some(scope) = state.scopes.get_mut(&root) else { return Ok(()); };
                scope.persist().await?;
                let owner = scope.topology.agent_for_session(session);
                scope.sessions.iter().any(|(&id, child)| {
                    owns_foreground(scope, owner, id)
                        && (child.active || scope.messages.has_pending_for(id)
                            || (!child.interrupted && (child.execution.is_some()
                                || matches!(child.status, AgentStatus::Pending | AgentStatus::Running | AgentStatus::Closing))))
                })
            };
            if !busy { return Ok(()); }
            revision.changed().await.map_err(std::io::Error::other)?;
        }
    }

    /// Cancels the caller's owned work before a failed or aborted parent settles.
    /// Explicit background subtrees retain their durable owners.
    pub async fn abort_foreground(&self, session: &str) -> std::io::Result<()> {
        {
            let state = self.state.lock().await;
            let root = state.root_session_id(session);
            if let Some(scope) = state.scopes.get(root)
                && let Some(owner) = scope.topology.agent_for_session(session)
                && scope.sessions.get(&owner).is_some_and(|child| child.interrupted)
            {
                // An ancestor already fenced the whole subtree and is joining
                // its drivers. Do not recursively wait on its admission lock.
                scope.ensure_readable()?;
                return Ok(());
            }
        }
        self.release_parent(session).await
    }

    /// Releases foreground work while retaining background trees and their
    /// ancestor factory checkpoints. Explicit close-all remains cancellation.
    pub async fn release_parent(&self, session: &str) -> std::io::Result<()> {
        let _messages = self.message_lock.lock().await;
        let (root, ids, harnesses, supporting) = {
            let mut state = self.state.lock().await;
            let root = state.root_session_id(session).to_owned();
            let Some(scope) = state.scopes.get(&root) else {
                // Tool installation registers the parent's factory before its
                // first child creates a scope. Retire that capability even for
                // an unused parent: an embedding factory can own this registry,
                // so keeping its handle here would form an ownership cycle.
                self.session_handles.write().expect("session handles poisoned").remove(&root);
                return Ok(());
            };
            let owner = scope.topology.agent_for_session(session);
            let background = scope.sessions.iter().filter_map(|(&id, child)| {
                (owner.is_none_or(|owner| scope.topology.is_descendant(id, owner)) && child.descriptor.lifetime == AgentLifetime::Background
                    && !matches!(child.status, AgentStatus::Closing | AgentStatus::Closed)).then_some(id)
            }).collect::<Vec<_>>();
            let mut ids = Vec::new();
            let mut supporting = Vec::new();
            for id in scope.topology.all_postorder() {
                if owner.is_some_and(|owner| !scope.topology.is_descendant(id, owner)) { continue; }
                if background.iter().any(|&bg| id == bg || scope.topology.is_descendant(id, bg)) { continue; }
                if background.iter().any(|&bg| scope.topology.is_descendant(bg, id)) { supporting.push(id); }
                else { ids.push(id); }
            }
            let harnesses = state.harnesses(&root, &ids, true)?;
            let scope = state.scope_mut(&root);
            for id in &ids { scope.sessions.get_mut(id).expect("child").interrupted = true; }
            for id in &supporting { scope.sessions.get_mut(id).expect("ancestor").interrupted = true; }
            scope.persist().await?;
            let supporting = supporting.into_iter().filter_map(|id| scope.sessions[&id].harness.clone()).collect::<Vec<_>>();
            (root, ids, harnesses, supporting)
        };
        // An ancestor is retained only as a factory capability. Its foreground
        // turn must stop even when a descendant outlives the parent.
        first_error(join_all(supporting.into_iter().map(|harness| async move { harness.interrupt().await })).await)?;
        for &id in &ids { self.send(&root, AgentUpdate::Status { id, status: AgentStatus::Closing }); }
        self.changed();
        self.stop_and_close(root.clone(), ids, harnesses).await?;
        if !self.has_background(&root).await {
            self.session_handles.write().expect("session handles poisoned").remove(&root);
        }
        Ok(())
    }

    /// Drops a poisoned generation's local capabilities without changing its
    /// durable checkpoint. A successor must recover the committed work; this
    /// is retirement after failed persistence, not a durable cancellation.
    /// Returns the root followed by its child session identities for host cleanup.
    pub async fn retire_failed_parent(&self, session: &str) -> Vec<String> {
        self.retire_local_parent(session, false).await
    }

    /// Retires a constructor that never published its parent capability. Even a
    /// healthy tree may have partially recovered before child acquisition failed.
    /// Abort local tasks only; committed work belongs to the next durable owner.
    pub async fn retire_unpublished_parent(&self, session: &str) -> Vec<String> {
        self.retire_local_parent(session, true).await
    }

    async fn retire_local_parent(&self, session: &str, unpublished: bool) -> Vec<String> {
        let _residency = self.residency_lock.lock().await;
        let _messages = self.message_lock.lock().await;
        let (sessions, tasks) = {
            let mut state = self.state.lock().await;
            let root = state.root_session_id(session).to_owned();
            // A published healthy background tree retains its owner. Constructor
            // failure has no published owner, regardless of journal health.
            if !unpublished && !state.scopes.get(&root).is_some_and(|scope| scope.observation_error.is_some()) {
                return Vec::new();
            }
            let mut sessions = vec![root.clone()];
            // enable_durability can fail after installing some identity mappings
            // but before publishing its scope.
            if !state.scopes.contains_key(&root) {
                sessions.extend(state.root_by_session.iter().filter_map(|(session, owner)| (owner == &root).then_some(session.clone())));
            }
            let mut tasks = Vec::new();
            for child in state.scopes.get_mut(&root).into_iter().flat_map(|scope| scope.sessions.values_mut()) {
                sessions.push(child.descriptor.session_id.clone());
                for task in child.harness_task.take().into_iter().chain(child.event_task.take()) {
                    task.abort();
                    tasks.push(task);
                }
                child.harness = None;
            }
            let mut handles = self.session_handles.write().expect("session handles poisoned");
            for session in &sessions { handles.remove(session); }
            if unpublished {
                state.scopes.remove(&root);
                state.root_by_session.retain(|_, owner| owner != &root);
            }
            (sessions, tasks)
        };
        // Do not run normal close callbacks: they would try to publish terminal
        // outcomes from an unpublished owner or an unusable journal.
        for task in tasks { let _ = task.await; }
        self.changed();
        sessions
    }

    pub(super) async fn admit_child_turn(
        &self, root: &str, id: AgentId, prompt: String, message_id: Option<MessageId>,
    ) -> std::io::Result<(u64, String, Option<String>, bool)> {
        let mut state = self.state.lock().await;
        let last_used = state.next_access();
        let scope = state.scopes.get_mut(root).ok_or_else(|| std::io::Error::other("child scope unavailable"))?;
        let durable = scope.journal.is_some();
        let child = scope.sessions.get_mut(&id).ok_or_else(|| std::io::Error::other("child unavailable"))?;
        if child.active || !child.status.can_start_turn() { return Err(std::io::Error::other("child cannot start a turn")); }
        let (revision, prompt, operation_id) = if durable {
            if child.execution.is_none() {
                let revision = child.next_instruction_revision.checked_add(1).ok_or_else(|| std::io::Error::other("child instruction revision exhausted"))?;
                child.next_instruction_revision = revision;
                child.execution = Some(ChildExecution {
                    operation_id: format!("child-turn:{id}:{revision}"), prompt, instruction_revision: revision,
                });
                child.submitted_output = None;
                child.interrupted = false;
            }
            let execution = child.execution.as_ref().expect("admission retained");
            (execution.instruction_revision, execution.prompt.clone(), Some(execution.operation_id.clone()))
        } else {
            child.interrupted = false;
            let revision = child.next_instruction_revision.checked_add(1).ok_or_else(|| std::io::Error::other("child instruction revision exhausted"))?;
            child.next_instruction_revision = revision;
            child.submitted_output = None;
            (revision, prompt, None)
        };
        let cancel_on_admission = child.interrupted;
        // The prompt revision identifies the original native admission. Accepted
        // steering advances completion authority without changing that input.
        child.active_instruction_revision = Some(child.active_instruction_revision.unwrap_or(revision));
        child.active = true;
        child.status = AgentStatus::Running;
        child.steering = false;
        child.last_used = last_used;
        if let Some(message_id) = message_id { scope.messages.mark_terminal(message_id); }
        scope.persist().await?;
        drop(state);
        self.send(root, AgentUpdate::Status { id, status: AgentStatus::Running });
        self.changed();
        Ok((revision, prompt, operation_id, cancel_on_admission))
    }
}

// Ownership is relative to the caller. A background parent's own foreground
// children remain owned by it while the outer root ignores that whole subtree.
fn owns_foreground(scope: &AgentScope, owner: Option<AgentId>, id: AgentId) -> bool {
    if owner.is_some_and(|owner| !scope.topology.is_descendant(id, owner)) { return false; }
    !scope.sessions.iter().any(|(&boundary, child)| {
        child.descriptor.lifetime == AgentLifetime::Background
            && owner.is_none_or(|owner| scope.topology.is_descendant(boundary, owner))
            && (boundary == id || scope.topology.is_descendant(id, boundary))
    })
}
