use eyre::{Result, WrapErr};
use harness_agent::{Agent, Thinking, Tool, ToolContext, ToolDefinition, ToolExecution, Tools};
use serde::{Deserialize, Serialize};
use serde_json::json;

struct Multiply;

#[derive(Deserialize)]
struct MultiplyInput {
    left: i64,
    right: i64,
}

#[derive(Serialize)]
struct MultiplyOutput {
    product: i64,
}

impl Tool for Multiply {
    type Input = MultiplyInput;

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "multiply",
            "Multiplies two signed integers.",
            json!({
                "type": "object",
                "properties": {
                    "left": { "type": "integer" },
                    "right": { "type": "integer" }
                },
                "required": ["left", "right"],
                "additionalProperties": false
            }),
        )
        .with_output_schema(json!({
            "type": "object",
            "properties": {
                "product": { "type": "integer" }
            },
            "required": ["product"],
            "additionalProperties": false
        }))
    }

    async fn execute(&self, input: MultiplyInput, _context: ToolContext<'_>) -> ToolExecution {
        match input.left.checked_mul(input.right) {
            Some(product) => ToolExecution::json(&MultiplyOutput { product }),
            None => ToolExecution::error("integer multiplication overflowed"),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("OPENAI_API_KEY").wrap_err("OPENAI_API_KEY is required")?;
    // Registered tools are exposed to code mode as `tools.<definition name>(args)`.
    let tools = Tools::builder().without_defaults().tool(Multiply).build()?;
    let (agent, mut events) = Agent::builder(api_key)
        .thinking(Thinking::Low)
        .tools(tools)
        .build()?;

    let turn = agent
        .prompt(
            "In code mode, call `await tools.multiply({ left: 6, right: 7 })` exactly once. Reply with only the product.",
        )
        .await?;
    events.write_turn_jsonl(std::io::stdout()).await?;
    let result = turn.result().await?;
    eprintln!("final result: {}", result.final_message);
    Ok(())
}
