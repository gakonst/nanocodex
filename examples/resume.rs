use eyre::{Result, WrapErr};
use nanocodex::{Nanocodex, SessionSnapshot, Thinking};

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("OPENAI_API_KEY").wrap_err("OPENAI_API_KEY is required")?;
    let workspace = std::env::current_dir().wrap_err("failed to resolve the workspace")?;

    let (agent, events) = Nanocodex::builder(api_key.clone())
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .build()?;
    drop(events);
    let completed = agent
        .prompt("Remember that the release codename is cobalt.")
        .await?
        .result()
        .await?;

    // The embedding application chooses the storage and retention policy.
    let stored = serde_json::to_vec(&completed.snapshot())?;
    drop((agent, completed));

    let snapshot: SessionSnapshot = serde_json::from_slice(&stored)?;
    let (resumed, events) = Nanocodex::builder(api_key)
        .thinking(Thinking::Low)
        .resume(snapshot)
        .build()?;
    drop(events);
    let result = resumed
        .prompt("What is the release codename? Reply with one word.")
        .await?
        .result()
        .await?;
    println!("{}", result.final_message);
    Ok(())
}
