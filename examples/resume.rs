use eyre::{Result, WrapErr};
use nanocodex::{Nanocodex, OpenAi, Thinking, agent::session::SessionSnapshot};

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("OPENAI_API_KEY").wrap_err("OPENAI_API_KEY is required")?;
    let workspace = std::env::current_dir().wrap_err("failed to resolve the workspace")?;
    let openai = OpenAi::new(api_key)?;

    let (agent, events) = Nanocodex::builder(openai.clone())
        .instructions("Remember explicit release facts and never infer missing values.")
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
    let snapshot = completed
        .snapshot()
        .ok_or_else(|| eyre::eyre!("the local agent did not retain a snapshot"))?;
    let stored = serde_json::to_vec(&snapshot)?;
    drop((agent, completed));

    let snapshot: SessionSnapshot = serde_json::from_slice(&stored)?;
    let (resumed, events) = Nanocodex::builder(openai)
        .instructions("Remember explicit release facts and never infer missing values.")
        .thinking(Thinking::Low)
        .resume(snapshot)
        .build()?;
    drop(events);
    let result = resumed
        .prompt("What is the release codename? Reply with one word.")
        .await?
        .result()
        .await?;
    println!("{}", result.final_message());
    Ok(())
}
