//! Drive a rented Popcorn browser session from a Nanocodex agent.
//!
//! The agent brain runs here; the browser runs inside a Popcorn TEE session
//! reached over CDP. The LiveView URL is printed so a human can watch the
//! session or take over when the agent gets stuck.
//!
//! ```sh
//! export OPENAI_API_KEY=...
//! export POPCORN_CONTROL_PLANE_URL=https://...
//! export POPCORN_CLIENT_ID=...
//! export POPCORN_CLIENT_SECRET=...
//! cargo run -p nanocodex-examples --bin popcorn-agent -- \
//!   "Open https://example.com, inspect the page, and report its main heading."
//! ```

use eyre::{Result, WrapErr};
use nanocodex::agent::events::{
    AgentEvent, AgentEventKind, AssistantMessage, ReasoningSummaryDelta,
};
use nanocodex::{AgentEvents, Nanocodex, OpenAi, Thinking, Tools};
use nanocodex_browser::popcorn::{PopcornBrowser, PopcornConfig};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::time::{Duration, timeout};

const TURN_SETTLE_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("OPENAI_API_KEY").wrap_err("OPENAI_API_KEY is required")?;
    let config = PopcornConfig::from_env()?;
    let popcorn = PopcornBrowser::spawn(config).await?;
    eprintln!("popcorn session: {}", popcorn.session().session_id);
    eprintln!("live view: {}", popcorn.live_view_url());

    let tools = Tools::builder().provider(popcorn.tool()).build()?;
    let openai = OpenAi::new(api_key)?;
    let (agent, mut events) = Nanocodex::builder(openai)
        .instructions(
            "Use `tools.browser` from Code Mode for browser work. The browser runs in a remote isolated session. Inspect the page after every navigation before interacting with it.",
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
    let popcorn_shutdown = popcorn.shutdown().await;
    let result = turn_result?;
    agent_shutdown?;
    popcorn_shutdown?;
    eprintln!("final result: {}", result.final_message());
    Ok(())
}

async fn write_turn_jsonl(
    events: &mut AgentEvents,
    output: &mut (impl AsyncWrite + Unpin),
) -> Result<()> {
    let mut echo = AgentEcho::default();
    while let Some(event) = events.recv().await {
        let terminal = event.kind.is_terminal();
        let mut record = serde_json::to_vec(&event)?;
        record.push(b'\n');
        output.write_all(&record).await?;
        output.flush().await?;
        echo.observe(&event);
        if terminal {
            echo.flush();
            return Ok(());
        }
    }
    Err(eyre::eyre!(
        "agent event stream closed before the turn emitted a terminal event"
    ))
}

/// Echoes the agent's own words to stderr so a human watching the terminal can
/// follow a turn while the JSONL contract on stdout stays byte-for-byte the same.
///
/// Only assistant messages and API-visible reasoning summaries are echoed. Tool
/// calls, tool results, and raw provider events are deliberately skipped.
#[derive(Default)]
struct AgentEcho {
    /// Reasoning-summary text received but not yet terminated by a newline.
    pending_summary: String,
}

impl AgentEcho {
    /// Echoes any human-readable text carried by one event.
    fn observe(&mut self, event: &AgentEvent) {
        match event.kind {
            AgentEventKind::AssistantMessage => {
                self.flush();
                if let Ok(message) = event.decode_payload::<AssistantMessage>() {
                    echo_lines(&message.text);
                }
            }
            AgentEventKind::ReasoningSummaryDelta => {
                let Ok(delta) = event.decode_payload::<ReasoningSummaryDelta>() else {
                    return;
                };
                self.pending_summary.push_str(&delta.text);
                while let Some(newline) = self.pending_summary.find('\n') {
                    let line = self.pending_summary[..newline].to_owned();
                    self.pending_summary.drain(..=newline);
                    echo_lines(&line);
                }
            }
            _ => {}
        }
    }

    /// Emits summary text that never received a trailing newline.
    fn flush(&mut self) {
        let pending = std::mem::take(&mut self.pending_summary);
        echo_lines(&pending);
    }
}

/// Writes one `agent:` line per non-blank line of `text`.
fn echo_lines(text: &str) {
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        eprintln!("agent: {line}");
    }
}
