//! Goal tools over host-owned persistence and live accounting.
//!
//! The host implements this adapter using its thread store and must flush live
//! usage before get/update. No process-global or in-memory pretend goal store.
use super::{ExtensionProvider, error};
use crate::{ToolContext, ToolOutput, ToolResult};
use serde::Deserialize;
use serde_json::Value;
#[async_trait::async_trait]
pub trait GoalStore: Send + Sync {
    async fn get(&self, context: ToolContext<'_>) -> Result<Value, crate::contract::ToolError>;
    async fn create(
        &self,
        objective: String,
        token_budget: Option<u64>,
        context: ToolContext<'_>,
    ) -> Result<Value, crate::contract::ToolError>;
    async fn update(
        &self,
        status: GoalStatus,
        context: ToolContext<'_>,
    ) -> Result<Value, crate::contract::ToolError>;
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Complete,
    Blocked,
    Paused,
}
pub struct GoalTools<S: GoalStore>(pub S);
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateArgs {
    objective: String,
    token_budget: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateArgs {
    status: GoalStatus,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}
#[async_trait::async_trait]
impl<S: GoalStore> ExtensionProvider for GoalTools<S> {
    fn names(&self) -> &'static [&'static str] {
        &["get_goal", "create_goal", "update_goal"]
    }
    async fn execute(&self, name: &str, input: Value, context: ToolContext<'_>) -> ToolResult {
        let value = match name {
            "get_goal" => {
                let _: EmptyArgs = serde_json::from_value(input)?;
                self.0.get(context).await?
            }
            "create_goal" => {
                let a: CreateArgs = serde_json::from_value(input)?;
                if a.objective.trim().is_empty()
                    || a.objective.trim().chars().count() > 4000
                    || a.token_budget == Some(0)
                {
                    return Err(error("invalid goal objective or token budget"));
                }
                self.0
                    .create(a.objective.trim().to_owned(), a.token_budget, context)
                    .await?
            }
            "update_goal" => {
                let a: UpdateArgs = serde_json::from_value(input)?;
                self.0.update(a.status, context).await?
            }
            _ => return Err(error("unknown goal tool")),
        };
        Ok(ToolOutput::json(&value))
    }
}

use serde::Serialize;
use serde_json::json;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadGoal {
    goal_id: String,
    thread_id: String,
    objective: String,
    status: String,
    token_budget: Option<u64>,
    tokens_used: u64,
    time_used_seconds: f64,
    created_at: u64,
    updated_at: u64,
}
#[derive(Default, Serialize, Deserialize)]
struct Persisted {
    goal: Option<ThreadGoal>,
    usage: BTreeMap<String, (u64, f64)>,
}
/// Durable JSON adapter for a single native thread. Keep one instance per thread;
/// call account_turn with cumulative uncached-input + output usage snapshots.
/// The caller owns continuation scheduling and explicit user resume controls.
pub struct FileGoalStore {
    path: PathBuf,
    thread_id: String,
    lock: Mutex<()>,
}
impl FileGoalStore {
    pub fn new(path: PathBuf, thread_id: String) -> Self {
        Self {
            path,
            thread_id,
            lock: Mutex::new(()),
        }
    }
    async fn load(&self) -> Result<Persisted, crate::contract::ToolError> {
        match tokio::fs::read(&self.path).await {
            Ok(bytes) => {
                let value: Persisted = serde_json::from_slice(&bytes)?;
                if value
                    .goal
                    .as_ref()
                    .is_some_and(|g| g.thread_id != self.thread_id)
                {
                    return Err(error("goal belongs to another thread"));
                }
                Ok(value)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Persisted::default()),
            Err(e) => Err(e.into()),
        }
    }
    async fn save(&self, value: &Persisted) -> Result<(), crate::contract::ToolError> {
        use tokio::io::AsyncWriteExt;
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let temporary = self
            .path
            .with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary).await?;
        file.write_all(&serde_json::to_vec(value)?).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temporary, &self.path).await?;
        Ok(())
    }
    fn bind(&self, context: ToolContext<'_>) -> Result<(), crate::contract::ToolError> {
        if context.session_id() != self.thread_id {
            return Err(error("goal context belongs to another thread"));
        }
        Ok(())
    }
    /// Idempotent cumulative accounting, bounded to the admitted goal identity.
    pub async fn account_turn(
        &self,
        goal_id: &str,
        turn: &str,
        tokens: u64,
        seconds: f64,
    ) -> Result<(), crate::contract::ToolError> {
        if !seconds.is_finite() || seconds < 0.0 {
            return Err(error("invalid goal usage"));
        }
        let _guard = self.lock.lock().await;
        let mut state = self.load().await?;
        let Some(goal) = &mut state.goal else {
            return Ok(());
        };
        if goal.goal_id != goal_id {
            return Ok(());
        }
        let previous = state.usage.get(turn).copied().unwrap_or((0, 0.0));
        let next = (tokens.max(previous.0), seconds.max(previous.1));
        goal.tokens_used = goal
            .tokens_used
            .checked_add(next.0 - previous.0)
            .ok_or_else(|| error("goal usage overflow"))?;
        goal.time_used_seconds += next.1 - previous.1;
        if !goal.time_used_seconds.is_finite() {
            return Err(error("goal usage overflow"));
        }
        if goal.token_budget.is_some_and(|b| goal.tokens_used >= b) && goal.status != "complete" {
            goal.status = "budgetLimited".into();
        }
        goal.updated_at = now();
        state.usage.insert(turn.to_owned(), next);
        self.save(&state).await
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
fn response(goal: Option<&ThreadGoal>, complete: bool) -> Value {
    json!({ "goal": goal,
        "remainingTokens": goal.and_then(|g| g.token_budget.map(|b| b.saturating_sub(g.tokens_used))),
        "completionBudgetReport": goal.filter(|_| complete).and_then(|g| g.token_budget.map(|b| format!("Goal completed using {} of {b} budgeted tokens.", g.tokens_used))),
    })
}
#[async_trait::async_trait]
impl GoalStore for FileGoalStore {
    async fn get(&self, context: ToolContext<'_>) -> Result<Value, crate::contract::ToolError> {
        self.bind(context)?;
        let _guard = self.lock.lock().await;
        Ok(response(self.load().await?.goal.as_ref(), false))
    }
    async fn create(
        &self,
        objective: String,
        token_budget: Option<u64>,
        context: ToolContext<'_>,
    ) -> Result<Value, crate::contract::ToolError> {
        self.bind(context)?;
        let _guard = self.lock.lock().await;
        let mut state = self.load().await?;
        if state.goal.as_ref().is_some_and(|g| g.status != "complete") {
            return Err(error(
                "cannot create a new goal because this thread has an unfinished goal",
            ));
        }
        let timestamp = now();
        state.goal = Some(ThreadGoal {
            goal_id: uuid::Uuid::new_v4().to_string(),
            thread_id: self.thread_id.clone(),
            objective,
            token_budget,
            status: "active".into(),
            tokens_used: 0,
            time_used_seconds: 0.0,
            created_at: timestamp,
            updated_at: timestamp,
        });
        state.usage.clear();
        self.save(&state).await?;
        Ok(response(state.goal.as_ref(), false))
    }
    async fn update(
        &self,
        status: GoalStatus,
        context: ToolContext<'_>,
    ) -> Result<Value, crate::contract::ToolError> {
        self.bind(context)?;
        let _guard = self.lock.lock().await;
        let mut state = self.load().await?;
        let goal = state
            .goal
            .as_mut()
            .ok_or_else(|| error("this thread has no goal"))?;
        if !matches!(goal.status.as_str(), "budgetLimited" | "usageLimited") {
            goal.status = match status {
                GoalStatus::Complete => "complete",
                GoalStatus::Blocked => "blocked",
                GoalStatus::Paused => "paused",
            }
            .into();
            goal.updated_at = now();
        }
        let complete = goal.status == "complete";
        self.save(&state).await?;
        Ok(response(state.goal.as_ref(), complete))
    }
}
