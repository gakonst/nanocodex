//! Owned, recoverable summaries progress alongside foreground Messages requests.
use super::*;
use futures_util::FutureExt;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct PendingSummary {
    pub(super) cutoff: Conversation,
    pub(super) step: String,
}
pub(super) struct SummaryWork<'a> {
    future: crate::execution::PolicyFuture<'a, (Conversation, Usage)>,
    pub(super) result: Option<Result<(Conversation, Usage)>>,
}
pub(super) async fn progress(work: &mut Option<SummaryWork<'_>>) -> Result<(Conversation, Usage)> {
    match work {
        Some(work) if work.result.is_none() => work.future.as_mut().await,
        _ => std::future::pending().await,
    }
}
pub(super) fn poll(work: &mut Option<SummaryWork<'_>>) {
    if let Some(work) = work
        && work.result.is_none()
    {
        work.result = work.future.as_mut().now_or_never();
    }
}
pub(super) async fn wait(work: &mut Option<SummaryWork<'_>>) {
    if let Some(work) = work
        && work.result.is_none()
    {
        work.result = Some(work.future.as_mut().await);
    }
}
impl State {
    pub(super) fn start_summary<'a>(
        &'a self,
        cursor: &Cursor,
        cancel: &'a Cancellation,
    ) -> Option<SummaryWork<'a>> {
        let pending = cursor.background.clone()?;
        let cursor = cursor.clone();
        Some(SummaryWork {
            result: None,
            future: Box::pin(async move {
                let mut context = pending.cutoff;
                let usage = self
                    .compact_locked(
                        &mut context,
                        cancel,
                        CompactionMode::Background,
                        &cursor,
                        &pending.step,
                    )
                    .await?;
                Ok((context, usage))
            }),
        })
    }
    pub(super) async fn install_summary(
        &self,
        cursor: &mut Cursor,
        work: &mut Option<SummaryWork<'_>>,
        context: &mut Conversation,
        pending: &mut Vec<Message>,
        usage: &mut Usage,
    ) -> Result<bool> {
        if !work.as_ref().is_some_and(|work| work.result.is_some()) {
            return Ok(false);
        }
        // Acquire shared discovery state before the ownership fence so the
        // context and discoveries swap without yielding after that fence.
        let mut discovered = self.discovered.lock().await;
        if let (Some(policy), Some(operation)) = (&self.policy, &cursor.operation) {
            policy.continuation(operation.clone()).await?;
        }
        let mut work = work.take().expect("completed work");
        let input = cursor.background.take().expect("owned cutoff");
        let (summary, cost) = work.result.take().expect("completed result")?;
        add_usage(usage, &cost);
        let cutoff = input.cutoff.packed_messages();
        if pending.len() < cutoff.len()
            || serde_json::to_value(&pending[..cutoff.len()]).map_err(provider_error)?
                != serde_json::to_value(&cutoff).map_err(provider_error)?
        {
            return Ok(false);
        }
        let mut messages = summary.messages;
        messages.extend(pending.iter().skip(cutoff.len()).cloned());
        context.messages = messages;
        context.summary = summary.summary;
        context.previous_message_id = None;
        context.auto_compaction_suppressed = true;
        context.rapid_compactions = summary.rapid_compactions;
        context.rounds_since_compaction = 0;
        *pending = context.packed_messages();
        context.active_context_tokens = estimate_text_tokens(&json!({"system":cursor.template.system,"tools":cursor.template.tools,"messages":pending}).to_string());
        if cursor.tool_search {
            let references = client_discovered_tools(pending);
            discovered.retain(|name| references.contains(name.as_str()));
        }
        Ok(true)
    }
}
