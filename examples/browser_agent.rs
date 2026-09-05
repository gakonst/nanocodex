use eyre::{Result, WrapErr};
use nanocodex::{AgentEvents, Nanocodex, OpenAi, Thinking, Tools};
use nanocodex_browser::{Browser, BrowserTool};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::time::{Duration, timeout};

const TURN_SETTLE_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("OPENAI_API_KEY").wrap_err("OPENAI_API_KEY is required")?;
    let browser = Browser::new()?;
    let tools = Tools::builder()
        .provider(BrowserTool::from_browser(browser.clone()))
        .build()?;
    let openai = OpenAi::new(api_key)?;
    let (agent, mut events) = Nanocodex::builder(openai)
        .instructions(
            "Use `tools.browser` from Code Mode for browser work. Inspect the page after every navigation before interacting with it.",
        )
        .thinking(Thinking::Low)
        .tools(tools)
        .build()?;

    let prompt = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let prompt = if prompt.is_empty() {
        "Open https://example.com, inspect the page, and report its main heading."
    } else {
        &prompt
    };
    let turn = agent.prompt(prompt).await?;
    let control = turn.control();
    let mut stdout = tokio::io::stdout();
    let turn_result: Result<_> = {
        let events_result = write_turn_jsonl(&mut events, &mut stdout);
        let turn_result = turn.result();
        tokio::pin!(events_result);
        tokio::pin!(turn_result);
        tokio::select! {
        result = &mut turn_result => {
            let terminal = timeout(TURN_SETTLE_TIMEOUT, &mut events_result).await;
            let result = result?;
            terminal
                .wrap_err("terminal event did not settle after the turn completed")??;
            Ok(result)
        }
        result = &mut events_result => match result {
            Ok(()) => Ok(timeout(TURN_SETTLE_TIMEOUT, &mut turn_result)
                .await
                .wrap_err("turn result did not settle after its terminal event")??),
            Err(event_error) => {
                let _ = timeout(TURN_SETTLE_TIMEOUT, control.cancel()).await;
                match timeout(TURN_SETTLE_TIMEOUT, &mut turn_result).await {
                    Ok(Err(turn_error)) => Err(turn_error.into()),
                    _ => Err(event_error),
                }
            }
        },
        }
    };
    let agent_shutdown = agent.shutdown().await;
    drop(agent);
    drop(events);
    let browser_shutdown = browser.close().await;
    let result = turn_result?;
    agent_shutdown?;
    browser_shutdown?;
    eprintln!("final result: {}", result.final_message());
    Ok(())
}

async fn write_turn_jsonl(
    events: &mut AgentEvents,
    output: &mut (impl AsyncWrite + Unpin),
) -> Result<()> {
    while let Some(event) = events.recv().await {
        let terminal = event.kind.is_terminal();
        let mut record = serde_json::to_vec(&event)?;
        record.push(b'\n');
        output.write_all(&record).await?;
        output.flush().await?;
        if terminal {
            return Ok(());
        }
    }
    Err(eyre::eyre!(
        "agent event stream closed before the turn emitted a terminal event"
    ))
}
