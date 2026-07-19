use eyre::{Result, WrapErr};
use nanocodex::{Nanocodex, Thinking, Tools, tool};

#[tool(description = "Multiplies two signed integers.")]
async fn multiply(left: i64, right: i64) -> std::result::Result<i64, &'static str> {
    left.checked_mul(right)
        .ok_or("integer multiplication overflowed")
}

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("OPENAI_API_KEY").wrap_err("OPENAI_API_KEY is required")?;
    // Registered tools are exposed to code mode as `tools.<definition name>(args)`.
    let tools = Tools::builder().without_defaults().tool(multiply).build()?;
    let (agent, mut events) = Nanocodex::builder(api_key)
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
