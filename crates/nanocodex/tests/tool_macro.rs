use nanocodex::{Tool, ToolContext, ToolInput, tool};
use serde_json::{Value, json, value::to_raw_value};

#[tool(name = "add_numbers", description = "Adds two signed integers.")]
async fn add(left: i64, right: i64) -> Result<i64, &'static str> {
    left.checked_add(right).ok_or("integer addition overflowed")
}

#[tokio::test]
async fn macro_generates_schema_and_executes_through_public_tool_trait() {
    assert_eq!(add.name(), "add_numbers");
    let definition = serde_json::to_value(add.definition()).unwrap();
    assert_eq!(definition["name"], "add_numbers");
    assert_eq!(definition["parameters"]["type"], "object");
    assert_eq!(
        definition["parameters"]["required"],
        json!(["left", "right"])
    );
    assert_eq!(definition["output_schema"]["type"], "integer");

    let execution = add
        .execute(
            ToolInput::Function(to_raw_value(&json!({ "left": 20, "right": 22 })).unwrap()),
            ToolContext {
                model: "test-model",
                session_id: "test-session",
                call_id: "test-call",
                history: &[],
                output_token_budget: nanocodex::DEFAULT_TOOL_OUTPUT_TOKENS,
            },
        )
        .await;
    assert!(execution.success);
    assert_eq!(
        serde_json::to_value(execution.output).unwrap(),
        Value::String("42".into())
    );
}
