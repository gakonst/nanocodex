//! Optional portable execution policy for native xAI checkpoints and effects.
//!
//! Hosts may implement this seam directly or use `nanocodex-durability`'s
//! fenced stores. Started effects without receipts are never automatically
//! repeated: the recovered request fails with uncertainty retained in history,
//! and a new caller prompt is required to continue safely.
use super::*;
use serde::{Deserialize, Serialize};

/// Future returned by a native host execution policy.
#[cfg(not(target_family = "wasm"))]
pub type PolicyFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;
/// Future returned by an isolate-local host execution policy.
#[cfg(target_family = "wasm")]
pub type PolicyFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + 'a>>;

/// Authoritative admission of an identified caller request.
pub enum Admission {
    /// Newly accepted request.
    Execute,
    /// Previously accepted request with an unfinished continuation.
    Resume,
    /// Exact terminal receipt; replay must not rewind current history.
    Completed { checkpoint: Value, output: Value },
    /// Exact retained terminal failure.
    Failed { checkpoint: Value, error: String },
    /// Previously cancelled request.
    Cancelled,
}
/// Admission of an external effect.
pub enum Step {
    /// The effect has not started and may execute once.
    Execute,
    /// Reuse the exact committed receipt without invoking the external effect.
    Replay(Value),
    /// A prior attempt started without a known receipt. Do not execute again.
    Uncertain,
}
/// Host-owned durable execution policy.
///
/// All writes must enforce owner fencing and compare-and-swap revisions. A
/// mutation failure of unknown commit status must be returned as an execution
/// policy error requiring reopen, never converted to a terminal success.
pub trait XaiExecutionPolicy: Send + Sync {
    fn state_id(&self) -> &str;
    fn admit(
        &self,
        id: String,
        input: Value,
        automatic: bool,
    ) -> PolicyFuture<'_, (String, Admission)>;
    fn begin_attempt(&self, id: String) -> PolicyFuture<'_, ()>;
    fn continuation(&self, id: String) -> PolicyFuture<'_, Option<Value>>;
    fn advance(&self, id: String, state: Value) -> PolicyFuture<'_, ()>;
    fn begin_step(
        &self,
        id: String,
        step_id: String,
        kind: String,
        input: Value,
    ) -> PolicyFuture<'_, Step>;
    fn complete_step(&self, id: String, step_id: String, output: Value) -> PolicyFuture<'_, ()>;
    fn complete(&self, id: String, checkpoint: Value, output: Value) -> PolicyFuture<'_, ()>;
    fn fail(&self, id: String, checkpoint: Value, error: String) -> PolicyFuture<'_, ()>;
    fn cancel(&self, id: String, checkpoint: Value) -> PolicyFuture<'_, ()>;
    fn release(&self, id: String) -> PolicyFuture<'_, ()>;
    fn shutdown(&self) -> PolicyFuture<'_, ()>;
    fn checkpoint(&self, state: Value) -> PolicyFuture<'_, ()>;
}

/// Only a fully received HTTP rejection permits another physical request.
/// Transport/stream failures are deliberately outside this receipt type.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum SampleReceipt {
    Completed { response: Value },
    Rejected { message: String },
}
impl SampleReceipt {
    pub(crate) fn into_result(self) -> Result<Value> {
        match self {
            Self::Completed { response } => Ok(response),
            Self::Rejected { message } => Err(invalid(message)),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Snapshot {
    provider: String,
    version: u32,
    pub(crate) history: Vec<Value>,
}
impl Snapshot {
    pub(crate) fn new(history: Vec<Value>) -> Self {
        Self {
            provider: "xai".into(),
            version: 1,
            history,
        }
    }
    pub(crate) fn decode(value: Value) -> Result<Self> {
        let snapshot: Self = serde_json::from_value(value).map_err(recovery_error)?;
        if snapshot.provider != "xai" || snapshot.version != 1 {
            return Err(recovery_error("unsupported checkpoint provider/version"));
        }
        Ok(snapshot)
    }
    pub(crate) fn encode(&self) -> Result<Value> {
        serde_json::to_value(self).map_err(error)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Effect {
    kind: String,
    input: Value,
    output: Option<Value>,
}

/// The baseline is advanced only after an entire model/tool round. Receipts
/// reconstruct any partially processed round without appending output twice.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Cursor {
    pub(crate) snapshot: Snapshot,
    pub(crate) index: usize,
    pub(crate) usage: Option<TurnUsage>,
    pub(crate) prepared: bool,
    pub(crate) model: String,
    pub(crate) thinking: Thinking,
    pub(crate) max_steps: usize,
    pub(crate) max_retries: usize,
    pub(crate) repetition_limit: usize,
    pub(crate) repetitions: HashMap<String, usize>,
    pub(crate) context_window_tokens: u64,
    pub(crate) compact_percent: u32,
    pub(crate) keep_tail: usize,
    #[serde(default)]
    pub(crate) compactions: usize,
    pub(crate) request_template: Value,
    operation: Option<String>,
    effects: HashMap<String, Effect>,
}
impl Cursor {
    pub(crate) async fn open(
        config: &Xai,
        operation: Option<&str>,
        history: &[Value],
    ) -> Result<Self> {
        if let (Some(policy), Some(operation)) = (&config.policy, operation)
            && let Some(value) = policy.continuation(operation.to_owned()).await?
        {
            let cursor: Self = serde_json::from_value(value).map_err(recovery_error)?;
            if cursor.operation.as_deref() != Some(operation) {
                return Err(recovery_error("continuation operation mismatch"));
            }
            Snapshot::decode(cursor.snapshot.encode()?)?;
            return Ok(cursor);
        }
        let mut tools: Vec<Value> = config
            .tools
            .values()
            .map(|(d, _)| {
                json!({
                    "type":"function", "name":d.name, "description":d.description,
                    "parameters":d.parameters
                })
            })
            .collect();
        tools.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        tools.extend(config.hosted.clone());
        Ok(Self {
            snapshot: Snapshot::new(history.to_vec()),
            index: 0,
            usage: None,
            prepared: false,
            model: config.model.clone(),
            thinking: config.thinking,
            max_steps: config.max_steps,
            max_retries: config.max_retries,
            repetition_limit: config.repetition_limit,
            repetitions: HashMap::new(),
            context_window_tokens: config.context_window_tokens,
            compact_percent: config.compact_percent,
            keep_tail: config.keep_tail,
            compactions: 0,
            request_template: json!({"model":config.model,"tools":tools,
                "reasoning":{"effort":effort(config.thinking)?,"summary":"concise"}}),
            operation: operation.map(str::to_owned),
            effects: HashMap::new(),
        })
    }
    pub(crate) fn has_step(&self, step: &str) -> bool {
        self.effects.contains_key(step)
    }
    pub(crate) fn has_effects(&self) -> bool {
        !self.effects.is_empty()
    }
    pub(crate) async fn advance(&mut self, config: &Xai) -> Result<()> {
        if let (Some(policy), Some(operation)) = (&config.policy, &self.operation) {
            policy
                .advance(
                    operation.clone(),
                    serde_json::to_value(&*self).map_err(error)?,
                )
                .await?;
        }
        Ok(())
    }
    pub(crate) async fn begin(
        &mut self,
        config: &Xai,
        step: &str,
        kind: &str,
        input: Value,
    ) -> Result<Step> {
        let (Some(policy), Some(operation)) = (&config.policy, self.operation.clone()) else {
            return Ok(Step::Execute);
        };
        if let Some(effect) = self.effects.get(step) {
            if effect.input != input || effect.kind != kind {
                return Err(recovery_error(
                    "effect identity reused with different input",
                ));
            }
            if let Some(output) = &effect.output {
                return Ok(Step::Replay(output.clone()));
            }
            // A receipt may have committed immediately before losing the cursor
            // acknowledgement. Query the authoritative step before declaring it
            // uncertain, but NEVER treat another Execute as permission to retry.
            return match policy
                .begin_step(operation, step.into(), kind.into(), input)
                .await?
            {
                Step::Replay(output) => {
                    self.effects.get_mut(step).expect("retained effect").output =
                        Some(output.clone());
                    Ok(Step::Replay(output))
                }
                Step::Execute | Step::Uncertain => Ok(Step::Uncertain),
            };
        }
        self.effects.insert(
            step.into(),
            Effect {
                kind: kind.into(),
                input: input.clone(),
                output: None,
            },
        );
        self.advance(config).await?;
        let admission = policy
            .begin_step(operation, step.into(), kind.into(), input)
            .await?;
        if let Step::Replay(output) = &admission {
            self.effects.get_mut(step).expect("staged effect").output = Some(output.clone());
        }
        Ok(admission)
    }
    pub(crate) async fn complete(&mut self, config: &Xai, step: &str, output: Value) -> Result<()> {
        let (Some(policy), Some(operation)) = (&config.policy, &self.operation) else {
            return Ok(());
        };
        if let Some(prior) = self
            .effects
            .get(step)
            .and_then(|effect| effect.output.as_ref())
        {
            if *prior != output {
                return Err(recovery_error("replayed receipt changed"));
            }
            return Ok(());
        }
        policy
            .complete_step(operation.clone(), step.into(), output.clone())
            .await?;
        self.effects
            .get_mut(step)
            .ok_or_else(|| recovery_error("missing started effect"))?
            .output = Some(output);
        self.advance(config).await
    }
    pub(crate) async fn finish_round(
        &mut self,
        config: &Xai,
        history: &[Value],
        usage: Option<TurnUsage>,
    ) -> Result<()> {
        self.snapshot = Snapshot::new(history.to_vec());
        self.usage = usage;
        self.index = self
            .index
            .checked_add(1)
            .ok_or_else(|| recovery_error("model round overflow"))?;
        self.effects.clear();
        self.advance(config).await
    }
}

pub(crate) async fn settle(
    config: &Xai,
    operation: Option<&str>,
    history: &[Value],
    result: &Result<TurnResult>,
) -> Result<()> {
    let (Some(policy), Some(operation)) = (&config.policy, operation) else {
        return Ok(());
    };
    if result
        .as_ref()
        .err()
        .is_some_and(|e| e.execution_policy_disposition().is_some())
    {
        return Ok(());
    }
    let checkpoint = Snapshot::new(history.to_vec()).encode()?;
    match result {
        Ok(result) => {
            policy
                .complete(
                    operation.into(),
                    checkpoint,
                    json!({"final_message":result.final_message(),"usage":result.usage()}),
                )
                .await
        }
        Err(NanocodexError::TurnCancelled) => policy.cancel(operation.into(), checkpoint).await,
        Err(error) => {
            policy
                .fail(operation.into(), checkpoint, error.to_string())
                .await
        }
    }
}
pub(crate) fn replay(operation: String, output: Value) -> Result<TurnResult> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Output {
        final_message: String,
        usage: Option<TurnUsage>,
    }
    let output: Output = serde_json::from_value(output).map_err(recovery_error)?;
    Ok(TurnResult::from_backend(
        Some(operation),
        output.final_message,
        output.usage,
    ))
}
pub(crate) fn candidate_id(kind: &str) -> String {
    format!("xai-{kind}-{}", uuid::Uuid::new_v4())
}
pub(crate) fn recovery_error(message: impl std::fmt::Display) -> NanocodexError {
    NanocodexError::execution_policy_with_disposition(
        "xAI recovery",
        nanocodex_agent::ExecutionPolicyDisposition::Reopen,
        error(message),
    )
}

/// Release the durable model owner after all active local work has stopped.
pub(crate) async fn shutdown(config: &Xai) -> Result<()> {
    if let Some(policy) = &config.policy {
        policy.shutdown().await?;
    }
    Ok(())
}
