use nanocodex_oai_api::tools::ToolDefinition;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;

use super::{StandardTool, Tool, ToolContext, ToolInput, ToolOutput, ToolResult};

/// Host-owned standard plan tool for runtimes that replace workspace effects.
pub struct UpdatePlanTool {
    current: Mutex<Option<UpdatePlanArgs>>,
}

impl UpdatePlanTool {
    /// Creates an empty retained plan.
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
    fn definition(&self) -> ToolDefinition {
        StandardTool::UpdatePlan.definition()
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let plan = input.decode_json::<UpdatePlanArgs>()?;
        tracing::debug!(
            explanation = ?plan.explanation,
            step_count = plan.plan.len(),
            "updating plan"
        );
        for (index, item) in plan.plan.iter().enumerate() {
            tracing::debug!(
                index,
                step = item.step,
                status = ?item.status,
                "updated plan item"
            );
        }
        *self.current.lock().await = Some(plan);
        Ok(ToolOutput::text("Plan updated").with_structured_result(json!({})))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdatePlanArgs {
    #[serde(default)]
    explanation: Option<String>,
    plan: Vec<PlanItem>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanItem {
    step: String,
    status: PlanStatus,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PlanStatus {
    Pending,
    InProgress,
    Completed,
}
