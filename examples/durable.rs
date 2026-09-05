use eyre::{OptionExt, Result, WrapErr};
use nanocodex::{
    DurableAgentExt, Nanocodex, OpenAi, Tools,
    durability::{DurableSession, MemoryStore},
    tool,
};

#[tool(description = "Multiplies two signed integers.")]
async fn multiply(left: i64, right: i64) -> std::result::Result<i64, &'static str> {
    left.checked_mul(right)
        .ok_or("integer multiplication overflowed")
}

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("OPENAI_API_KEY").wrap_err("OPENAI_API_KEY is required")?;

    // These remain independently useful lower-level components.
    let openai = OpenAi::new(api_key)?;
    let tools = Tools::builder().without_defaults().tool(multiply).build()?;

    // MemoryStore demonstrates the host contract in one process. Production
    // hosts can provide the same compare-and-append contract or use SQLite or
    // Postgres through nanocodex-durability's optional features.
    let store = MemoryStore::new()?;
    let journal = DurableSession::open(store, "durable-example").await?;
    let (agent, _events) = Nanocodex::builder(openai)
        .instructions("Use the supplied arithmetic tool and preserve exact integer results.")
        .tools(tools)
        .durability(journal)
        .await?
        .build()?;

    // No request ID is required. Durable admission generates one, and the
    // returned Turn and TurnResult expose the selected durable identity.
    let turn = agent
        .prompt("Use the multiply tool once for 6 times 7. Reply with only the product.")
        .await?;
    let request_id = turn
        .request_id()
        .ok_or_eyre("durable prompt did not receive a request ID")?
        .to_owned();
    let result = turn.await?;

    assert_eq!(result.request_id(), Some(request_id.as_str()));
    println!("{request_id}: {}", result.final_message());
    agent.shutdown().await?;
    Ok(())
}
