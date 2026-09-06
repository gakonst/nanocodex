use super::*;
use nanocodex_tools::context_management::ContextManagement;

impl<S> ModelRun<S>
where
    S: Service<ResponsesAttempt, Response = ResponsesServiceResponse> + AgentSend + 'static,
    S::Error: Into<nanocodex_oai_api::ResponseError>,
    S::Future: AgentSend,
{
    pub(super) async fn initialize_context_management(&mut self) -> Result<()> {
        if self.context_management_checked {
            return Ok(());
        }
        #[cfg(target_family = "wasm")]
        let host = self
            .session
            .as_ref()
            .and_then(|session| session.tools.history_notes_host())
            .or_else(|| nanocodex_tools::embedded::history_notes_host(&self.tools));
        #[cfg(target_family = "wasm")]
        let eligible = if self.config.experimental_context && self.model == Model::Astra {
            match &host {
                Some(host) => {
                    host.eligible(
                        self.config.auth.clone(),
                        self.config.api_base_url.clone(),
                        self.events.request_id().to_owned(),
                    )
                    .await
                }
                None => false,
            }
        } else {
            false
        };
        #[cfg(not(target_family = "wasm"))]
        let eligible = self.config.experimental_context
            && ContextManagement::eligible(
                self.model,
                &self.config.auth,
                &self.config.api_base_url,
            )
            .await;
        if !eligible {
            self.context_management_checked = true;
            return Ok(());
        }
        let history = self
            .session
            .as_ref()
            .map_or_else(Vec::new, |session| session.conversation.flattened_history());
        // All branches share the provider session, but each thread owns its notes.
        let agent_name = if self.provider_session_id.as_ref() == self.events.request_id() {
            "/root".to_owned()
        } else {
            format!("/root/{}", self.events.request_id())
        };
        let context = ContextManagement::new(
            self.config.auth.clone(),
            self.config.api_base_url.clone(),
            self.provider_session_id.to_string(),
            agent_name,
            &history,
        );
        #[cfg(target_family = "wasm")]
        let context = context.with_host(
            host.expect("eligible host"),
            self.events.request_id().to_owned(),
        );
        if let Some(mut session) = self.session.take() {
            if let Err(error) = context.install(&mut session.tools) {
                self.session = Some(session);
                return Err(NanocodexError::InvalidExecutionPolicy(error));
            }
            session.factory = match self.attempt_factory(&session.tools) {
                Ok(factory) => factory,
                Err(error) => {
                    self.session = Some(session);
                    return Err(error);
                }
            };
            if !has_context_window(&history) {
                session.conversation.append(context.initial_context().await);
            }
            session.conversation.reset_for_full_request();
            self.session = Some(session);
        }
        self.context_management = Some(context);
        self.context_management_checked = true;
        Ok(())
    }

    pub(super) async fn maybe_reset_context(
        &mut self,
        index: u32,
        conversation: &mut ConversationState,
        factory: &ResponsesAttemptFactory,
        snapshot: Option<&ContextSnapshot>,
    ) -> Result<bool> {
        let Some(context) = self.context_management.clone() else {
            return Ok(false);
        };
        let used = conversation.active_context_tokens();
        let base = self.config.context_window_tokens.saturating_mul(9) / 10;
        let remaining = base.saturating_sub(used);
        context.set_remaining(remaining);
        let hard_limit = (base + context.budget().auto_compact_fallback_buffer_tokens)
            .min(self.config.context_window_tokens.saturating_mul(95) / 100);
        // Durable tool replay does not execute the handler again. Observe the
        // completed new_context call in retained history as well as live state.
        let history = conversation.flattened_history();
        let requested_in_history = completed_context_request(&history);
        // The handler can run before its durable tool result is committed.
        // Only an acknowledged result authorizes resetting retained history.
        context.take_request();
        if self.force_compaction || requested_in_history || used >= hard_limit {
            return self
                .reset_context(index, conversation, factory, snapshot)
                .await;
        }
        let budget = context.budget();
        let mut injected = false;
        if remaining <= budget.reminder_threshold_tokens
            && !contains_text(&history, "Your current context window is nearly exhausted;")
        {
            conversation.append([developer_message(
                budget
                    .reminder_message_template
                    .replace("{n_remaining}", &remaining.to_string()),
            )]);
            injected = true;
        }
        if remaining == 0 && !contains_text(&history, &budget.auto_compact_fallback_prompt) {
            conversation.append([developer_message(
                budget.auto_compact_fallback_prompt.clone(),
            )]);
            injected = true;
        }
        if injected {
            // Pre-turn preparation clears the old continuation delta after
            // this check. Keep new guidance in the next full request, even
            // when no context reset was needed.
            conversation.reset_for_full_request();
        }
        Ok(false)
    }

    pub(super) async fn reset_context(
        &mut self,
        index: u32,
        conversation: &mut ConversationState,
        factory: &ResponsesAttemptFactory,
        snapshot: Option<&ContextSnapshot>,
    ) -> Result<bool> {
        let Some(context) = self.context_management.clone() else {
            return Ok(false);
        };
        let started = Instant::now();
        self.events.emit(
            AgentEventKind::ModelCompactionStarted,
            CompactionStarted {
                after_model_call_index: index,
                active_context_tokens: conversation.active_context_tokens(),
                auto_compact_token_limit: self.config.context_window_tokens * 9 / 10,
                previous_response_id: conversation.previous_response_id(),
            },
        )?;
        let step_id = format!("context-window-{index}");
        let steps = self.execution_steps.clone();
        let previous = context.window();
        let input = serde_json::json!({"previous_context_window_id": previous.context_window_id});
        let recovered = if let Some(steps) = &steps {
            match steps
                .begin::<_, Vec<ResponseItem>>(&step_id, "context_window", &input)
                .await?
            {
                crate::agent::ExecutionStep::Execute => None,
                crate::agent::ExecutionStep::Replay(output) => Some(output),
            }
        } else {
            None
        };
        let items = if let Some(items) = recovered {
            items
        } else {
            let next = context.successor();
            let canonical = snapshot.map_or_else(
                || (*conversation.canonical_context).clone(),
                ContextSnapshot::full_item,
            );
            let mut items = vec![developer_context(), canonical];
            items.extend(next.initial_context().await);
            // Client-authored developer instructions survive resets. User/task
            // history is recovered with notes/history, as in upstream Codex.
            items.extend(compaction::truncate_retained_messages(
                conversation
                    .flattened_history()
                    .into_iter()
                    .filter(|item| {
                        is_client_developer(item)
                            && !contains_text(
                                std::slice::from_ref(item),
                                "Your current context window is nearly exhausted;",
                            )
                            && !contains_text(
                                std::slice::from_ref(item),
                                &context.budget().auto_compact_fallback_prompt,
                            )
                    })
                    .collect(),
                64_000,
            ));
            if let Some(steps) = &steps {
                steps.complete(&step_id, &items).await?;
            }
            items
        };
        context.restore(&items);
        conversation
            .managed
            .start_context_window(items, factory.profile().prefix());
        self.force_compaction = false;
        self.stats.compactions += 1;
        self.events.emit(
            AgentEventKind::ModelCompactionCompleted,
            CompactionCompleted {
                after_model_call_index: index,
                response_id: &context.window().context_window_id,
                attempt: 0,
                connection_generation: 0,
                status: "completed",
                duration_ns: elapsed_ns(started),
                time_to_first_event_ns: 0,
                time_to_first_output_ns: None,
                usage: None,
            },
        )?;
        Ok(true)
    }
}

fn developer_message(text: String) -> ResponseItem {
    ResponseItem::message(
        MessageRole::Developer,
        [ContentItem::InputText { text: text.into() }],
    )
}
fn has_context_window(history: &[ResponseItem]) -> bool {
    contains_text(history, "<context_window>\n")
}
fn contains_text(history: &[ResponseItem], needle: &str) -> bool {
    history.iter().any(|item| matches!(item, ResponseItem::Message { content, .. } if content.iter().any(|part| matches!(part, ContentItem::InputText { text } if text.contains(needle)))))
}
fn completed_context_request(history: &[ResponseItem]) -> bool {
    history.iter().any(|item| {
        let ResponseItem::FunctionCall { name, call_id, .. } = item else {
            return false;
        };
        name.as_ref() == "new_context"
            && history.iter().any(|item| {
                matches!(item,
                    ResponseItem::FunctionCallOutput { call_id: output_id, output, .. }
                    if output_id == call_id && is_context_success(output)
                )
            })
    })
}
fn is_context_success(output: &nanocodex_oai_api::responses::FunctionOutputBody) -> bool {
    use nanocodex_oai_api::responses::{FunctionOutputBody, FunctionOutputContent};
    const SUCCESS: &str =
        "A new context window will start without summarizing conversation history.";
    match output {
        FunctionOutputBody::Text(text) => text.as_ref() == SUCCESS,
        FunctionOutputBody::Content(parts) => {
            matches!(parts.as_slice(), [FunctionOutputContent::InputText { text }] if text.as_ref() == SUCCESS)
        }
    }
}
fn is_client_developer(item: &ResponseItem) -> bool {
    let ResponseItem::Message {
        role: MessageRole::Developer,
        content,
        ..
    } = item
    else {
        return false;
    };
    !content.iter().any(|part| matches!(part, ContentItem::InputText { text } if ["<permissions instructions>", "<context_window>", "<context_window_guidance>", "<context_window_reminder>"].iter().any(|prefix| text.trim_start().starts_with(prefix))))
}

#[cfg(all(test, not(target_family = "wasm")))]
#[path = "context_management_test_policy.rs"]
mod test_policy;

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;
    use nanocodex_oai_api::responses::FunctionOutputBody;

    #[test]
    fn reset_replay_requires_a_successful_tool_result() {
        let call: ResponseItem = serde_json::from_value(serde_json::json!({"type":"function_call", "name":"new_context", "call_id":"reset", "arguments":"{}"})).unwrap();
        assert!(!completed_context_request(std::slice::from_ref(&call)));
        let failed = ResponseItem::function_call_output(
            "reset".into(),
            FunctionOutputBody::Text("cancelled".into()),
        );
        assert!(!completed_context_request(&[call.clone(), failed]));
        let success = ResponseItem::function_call_output(
            "reset".into(),
            FunctionOutputBody::Text(
                "A new context window will start without summarizing conversation history.".into(),
            ),
        );
        assert!(completed_context_request(&[call, success]));
    }

    #[tokio::test]
    async fn reset_preserves_tools_cache_and_client_instructions_without_a_model_call() {
        let workspace = tempfile::tempdir().unwrap();
        let (events, _stream) = EventSink::channel("context-test".into());
        let config = Arc::new(ModelConfig {
            context_window_tokens: 100_000,
            ..ModelConfig::default()
        });
        let context = ContextManagement::new(
            config.auth.clone(),
            "http://127.0.0.1:1".into(),
            "context-test".into(),
            "/root".into(),
            &[],
        );
        let first = context.window();
        let client = ResponsesClient::new(tower::service_fn(|_: ResponsesAttempt| async {
            panic!("a context reset must never request a summary");
            #[allow(unreachable_code)]
            Ok::<ResponsesServiceResponse, nanocodex_oai_api::ResponseError>(unreachable!())
        }));
        let mut run = ModelRun::new(
            events,
            Arc::from("context-test"),
            config,
            client,
            Arc::default(),
            Tools::builder().without_defaults().build().unwrap(),
            ModelPromptCache::new(Arc::from("stable-cache"), None),
            crate::agent::ContextSourceConfig::default().build(),
            None,
        );
        run.context_management = Some(context.clone());
        run.context_management_checked = true;
        let mut session = run.empty_session(workspace.path().to_str()).unwrap();
        let tool_context = ToolContext::new("gpt-6-astra", "context-test", "code", &[], 10_000);
        assert!(
            session
                .tools
                .execute_code("store('checkpoint', 42);", tool_context)
                .await
                .success
        );
        session.conversation.append(context.initial_context().await);
        session.conversation.append([
            developer_message("Keep this client instruction".into()),
            ResponseItem::message(
                MessageRole::User,
                [ContentItem::InputText {
                    text: "Previous user task".into(),
                }],
            ),
            developer_message(context.budget().auto_compact_fallback_prompt.clone()),
        ]);
        let prefix = serde_json::to_value(session.factory.profile().prefix()).unwrap();
        let journal = Arc::new(test_policy::Journal::default());
        run.execution_steps = Some(ExecutionSteps::for_test(journal.clone()));
        let before = serde_json::to_value(session.conversation.flattened_history()).unwrap();
        // Simulate losing the acknowledgement after the reset step was saved.
        journal
            .lose_ack
            .store(true, std::sync::atomic::Ordering::Release);
        assert!(
            run.reset_context(1, &mut session.conversation, &session.factory, None)
                .await
                .is_err()
        );
        assert_eq!(context.window().context_window_id, first.context_window_id);
        assert_eq!(
            serde_json::to_value(session.conversation.flattened_history()).unwrap(),
            before
        );
        run.reset_context(1, &mut session.conversation, &session.factory, None)
            .await
            .unwrap();
        let history = session.conversation.flattened_history();
        assert!(contains_text(&history, "Keep this client instruction"));
        assert!(!contains_text(&history, "Previous user task"));
        assert!(!contains_text(
            &history,
            &context.budget().auto_compact_fallback_prompt
        ));
        assert_eq!(session.factory.profile().prompt_cache_key(), "stable-cache");
        assert_eq!(
            serde_json::to_value(session.factory.profile().prefix()).unwrap(),
            prefix
        );
        assert_eq!(
            context.window().previous_window_id.as_deref(),
            Some(first.context_window_id.as_str())
        );
        assert_eq!(context.window().window_number, 1);
        assert_eq!(context.window().first_window_id, first.first_window_id);
        assert!(session.conversation.previous_response_id().is_none());
        let restored = ContextManagement::new(
            run.config.auth.clone(),
            "http://127.0.0.1:1".into(),
            "context-test".into(),
            "/root".into(),
            &serde_json::from_slice::<Vec<ResponseItem>>(&serde_json::to_vec(&history).unwrap())
                .unwrap(),
        );
        assert_eq!(
            restored.window().context_window_id,
            context.window().context_window_id
        );
        assert_eq!(restored.window().window_number, 1);
        let output = session.tools.execute_code("if (load('checkpoint') !== 42) throw Error('environment reset'); text('retained');", tool_context).await;
        assert!(output.success);
        assert_eq!(run.stats.compactions, 1);

        // Stop at the journal boundary; a retained request must keep the item
        // IDs that the model and history service use in notes references.
        assert!(
            run.perform_model_call(2, &mut session.conversation, &session.factory)
                .await
                .is_err()
        );
        let retained = journal.input("model-2");
        let retained: serde_json::Value = serde_json::from_str(&retained).unwrap();
        let expected = serde_json::to_value(
            session
                .conversation
                .prompt_history()
                .iter()
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(retained["prompt_history"], expected);
        assert!(
            retained["prompt_history"]
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item["id"].is_string())
        );
        session
            .conversation
            .append([developer_message("Unretained changes".into())]);
        assert!(
            run.perform_model_call(2, &mut session.conversation, &session.factory)
                .await
                .is_err()
        );
        assert_eq!(
            serde_json::to_value(
                session
                    .conversation
                    .prompt_history()
                    .iter()
                    .collect::<Vec<_>>()
            )
            .unwrap(),
            expected
        );

        // Pre-turn delta cleanup must not discard a newly injected reminder.
        run.execution_steps = None;
        session.conversation.update_token_info(Some(&Usage {
            input_tokens: 85_000,
            total_tokens: 85_000,
            ..Usage::default()
        }));
        session
            .conversation
            .set_previous_response_id("prior-response");
        let (_cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel();
        assert!(
            run.prepare_follow_on_turn(
                &mut session,
                &Prompt::new("Next user request"),
                &mut cancel_rx
            )
            .await
            .unwrap()
        );
        let history = session.conversation.flattened_history();
        assert!(contains_text(
            &history,
            "Your current context window is nearly exhausted;"
        ));
        assert!(contains_text(&history, "Next user request"));
        assert!(session.conversation.previous_response_id().is_none());
    }
}
