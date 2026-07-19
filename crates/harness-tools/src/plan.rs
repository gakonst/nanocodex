use harness_core::ToolDefinition;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;

use super::{Tool, ToolContext, ToolExecution, ToolInput};

pub(super) struct PlanHandler {
    current: Mutex<Option<UpdatePlanArgs>>,
}

impl PlanHandler {
    pub(super) const fn new() -> Self {
        Self {
            current: Mutex::const_new(None),
        }
    }
}

#[async_trait::async_trait]
impl Tool for PlanHandler {
    fn name(&self) -> &'static str {
        "update_plan"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            self.name(),
            "Updates the task plan.\nProvide an optional explanation and a list of plan items, each with a step and status.\nAt most one step can be in_progress at a time.\n",
            json!({
                "type": "object",
                "properties": {
                    "explanation": {
                        "type": "string",
                        "description": "Optional explanation for this plan update."
                    },
                    "plan": {
                        "type": "array",
                        "description": "The list of steps",
                        "items": {
                            "type": "object",
                            "properties": {
                                "step": { "type": "string", "description": "Task step text." },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"],
                                    "description": "Step status."
                                }
                            },
                            "required": ["step", "status"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["plan"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolExecution {
        let plan = match input.decode_json::<UpdatePlanArgs>() {
            Ok(plan) => plan,
            Err(error) => return ToolExecution::error(error.to_string()),
        };
        if plan
            .plan
            .iter()
            .filter(|item| matches!(item.status, PlanStatus::InProgress))
            .count()
            > 1
        {
            return ToolExecution::error("update_plan allows at most one in_progress step");
        }
        if plan.plan.iter().any(|item| item.step.trim().is_empty()) {
            return ToolExecution::error("update_plan steps must not be empty");
        }
        let _ = plan.explanation.as_deref();
        *self.current.lock().await = Some(plan);
        ToolExecution::text("Plan updated").with_code_mode_value(json!({}))
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
