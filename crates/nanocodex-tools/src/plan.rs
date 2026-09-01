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
        let active_steps = plan
            .plan
            .iter()
            .filter(|item| matches!(item.status, PlanStatus::InProgress))
            .count();
        if active_steps > 1 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "at most one plan step may be in_progress",
            )
            .into());
        }
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

#[cfg(test)]
mod tests {
    use nanocodex_oai_api::tools::DEFAULT_TOOL_OUTPUT_TOKENS;
    use serde_json::{json, value::to_raw_value};

    use super::*;

    fn context() -> ToolContext<'static> {
        ToolContext::new(
            "test-model",
            "test-session",
            "test-call",
            &[],
            DEFAULT_TOOL_OUTPUT_TOKENS,
        )
    }

    #[tokio::test]
    async fn rejects_more_than_one_in_progress_step() {
        let input = to_raw_value(&json!({
            "plan": [
                { "step": "first", "status": "in_progress" },
                { "step": "second", "status": "in_progress" }
            ]
        }))
        .unwrap();
        let result = UpdatePlanTool::new()
            .execute(ToolInput::Function(input), context())
            .await;
        let Err(error) = result else {
            panic!("update_plan accepted multiple in_progress steps");
        };

        assert_eq!(
            error.to_string(),
            "at most one plan step may be in_progress"
        );
    }

    #[tokio::test]
    async fn accepts_one_in_progress_step() {
        let input = to_raw_value(&json!({
            "plan": [
                { "step": "first", "status": "completed" },
                { "step": "second", "status": "in_progress" }
            ]
        }))
        .unwrap();
        let output = UpdatePlanTool::new()
            .execute(ToolInput::Function(input), context())
            .await
            .unwrap();

        assert!(output.success);
    }
}
