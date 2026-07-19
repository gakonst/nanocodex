use eyre::{Result, WrapErr};
use nanocodex::{Nanocodex, Thinking, Tools};

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("OPENAI_API_KEY").wrap_err("OPENAI_API_KEY is required")?;
    let tools = Tools::builder()
        .web_search(false)
        .image_generation(false)
        .build()?;
    let (agent, events) = Nanocodex::builder(api_key)
        .thinking(Thinking::Low)
        .tools(tools)
        .build()?;
    tokio::spawn(async move {
        let mut events = events;
        while let Some(event) = events.recv().await {
            eprintln!("event: {:?}", event.kind);
        }
    });

    let first = agent
        .prompt("Reply with exactly one lowercase word: cobalt.")
        .await?;
    eprintln!("first prompt accepted; the agent is running independently");

    let first = first.result().await?;
    println!("first result: {}", first.final_message);

    let second = agent
        .prompt(
            "What single word did you reply with in the previous turn? Reply with only that word in uppercase.",
        )
        .await?;
    let second = second.result().await?;
    println!("second result: {}", second.final_message);

    Ok(())
}
