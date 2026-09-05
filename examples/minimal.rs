use nanocodex::{Nanocodex, OpenAi};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENAI_API_KEY")?;
    let openai = OpenAi::new(api_key)?;
    let (agent, _) = Nanocodex::builder(openai)
        .instructions("Inspect the repository carefully and report only verified facts.")
        .build()?;

    let turn = agent
        .prompt("Inspect this repository and summarize it.")
        .await?;
    let result = turn.result().await?;
    println!("{}", result.final_message());
    Ok(())
}
