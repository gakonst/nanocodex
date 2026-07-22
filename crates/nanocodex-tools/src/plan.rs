use nanocodex_core::ToolDefinition;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;

use super::{StandardTool, Tool, ToolContext, ToolExecution, ToolInput, ToolResult};

/// Host-owned standard plan tool for runtimes that replace workspace effects.
pub struct UpdatePlanTool {
    current: Mutex<Option<UpdatePlanArgs>>,
}

impl UpdatePlanTool {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            current: Mutex::const_new(None),
        }
    }
}

impl Default for UpdatePlanTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for UpdatePlanTool {
    fn name(&self) -> &'static str {
        "update_plan"
    }

    fn definition(&self) -> ToolDefinition {
        StandardTool::UpdatePlan.definition()
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let plan = input.decode_json::<UpdatePlanArgs>()?;
        if plan
            .plan
            .iter()
            .filter(|item| matches!(item.status, PlanStatus::InProgress))
            .count()
            > 1
        {
            return Ok(ToolExecution::error(
                "update_plan allows at most one in_progress step",
            ));
        }
        if plan.plan.iter().any(|item| item.step.trim().is_empty()) {
            return Ok(ToolExecution::error("update_plan steps must not be empty"));
        }
        let _ = plan.explanation.as_deref();
        *self.current.lock().await = Some(plan);
        Ok(ToolExecution::text("Plan updated").with_code_mode_value(json!({})))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdatePlanArgs {
    #[serde(default)]
    explanation: Option<String>,
    plan: Vec<PlanItem>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanItem {
    step: String,
    status: PlanStatus,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum PlanStatus {
    Pending,
    InProgress,
    Completed,
}
