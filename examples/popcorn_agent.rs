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
//!
//! A literal `--` between two prompts runs them as two turns on the same agent
//! and the same rented session, pausing between them so a human can take over
//! the live view. The second turn resumes with whatever state the human left
//! behind, such as a completed login:
//!
//! ```sh
//! cargo run -p nanocodex-examples --bin popcorn-agent -- \
//!   "Open the site and stop at the login form." \
//!   -- "Open the first post in the feed and summarise it."
//! ```

use eyre::{Result, WrapErr};
use nanocodex::agent::events::{AgentEvent, AgentEventKind, AssistantMessage};
use nanocodex::{AgentEvents, Nanocodex, OpenAi, Thinking, Tools, TurnResult};
use nanocodex_browser::popcorn::{PopcornBrowser, PopcornConfig};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::time::{Duration, timeout};

const TURN_SETTLE_TIMEOUT: Duration = Duration::from_secs(5);

const DEFAULT_PROMPT: &str =
    "Open https://example.com, inspect the page, and report its main heading.";

#[tokio::main]
async fn main() -> Result<()> {
    // Resolve prompts before renting anything so a usage error cannot leak a session.
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let (first_prompt, second_prompt) = split_prompts(&args)?;
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

    let mut stdout = tokio::io::stdout();
    let turn_result = run_handoff(
        &agent,
        &mut events,
        &mut stdout,
        &first_prompt,
        second_prompt.as_deref(),
    )
    .await;
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

/// Splits arguments on a literal `--` into the pair of handoff prompts.
///
/// Without a separator the joined arguments are the only prompt, falling back
/// to [`DEFAULT_PROMPT`] when no arguments were supplied.
fn split_prompts(args: &[String]) -> Result<(String, Option<String>)> {
    let Some(separator) = args.iter().position(|arg| arg == "--") else {
        let prompt = args.join(" ");
        return Ok(if prompt.trim().is_empty() {
            (DEFAULT_PROMPT.to_owned(), None)
        } else {
            (prompt, None)
        });
    };
    let before = args[..separator].join(" ");
    let after = args[separator + 1..].join(" ");
    if before.trim().is_empty() || after.trim().is_empty() {
        eyre::bail!("`--` must separate two non-empty prompts");
    }
    Ok((before, Some(after)))
}

/// Runs the first prompt, then hands the session to a human before running the
/// second prompt on the same agent, browser, and rented session.
///
/// Returns the last turn's result so the caller reports one final message.
async fn run_handoff(
    agent: &Nanocodex,
    events: &mut AgentEvents,
    output: &mut (impl AsyncWrite + Unpin),
    first: &str,
    second: Option<&str>,
) -> Result<TurnResult> {
    let first_result = run_turn(agent, events, output, first).await?;
    let Some(second) = second else {
        return Ok(first_result);
    };
    echo_agent_lines(first_result.final_message());
    eprintln!("waiting for human: log in via the live view, then press Enter");
    wait_for_enter().await?;
    run_turn(agent, events, output, second).await
}

/// Blocks until the operator presses Enter.
async fn wait_for_enter() -> Result<()> {
    let mut line = String::new();
    let read = BufReader::new(tokio::io::stdin())
        .read_line(&mut line)
        .await
        .wrap_err("failed to read the handoff confirmation from stdin")?;
    if read == 0 {
        eyre::bail!("stdin closed before the human handoff was confirmed");
    }
    Ok(())
}

/// Runs one prompt to completion while streaming its events to `output`.
async fn run_turn(
    agent: &Nanocodex,
    events: &mut AgentEvents,
    output: &mut (impl AsyncWrite + Unpin),
    prompt: &str,
) -> Result<TurnResult> {
    let turn = agent.prompt(prompt).await?;
    let control = turn.control();
    let events_result = write_turn_jsonl(events, output);
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
        echo_assistant_message(&event);
        if terminal {
            return Ok(());
        }
    }
    Err(eyre::eyre!(
        "agent event stream closed before the turn emitted a terminal event"
    ))
}

/// Echoes one completed assistant message to stderr so a human watching the
/// terminal can follow a turn while the JSONL contract on stdout stays
/// byte-for-byte the same.
///
/// Every other event is skipped, including tool calls, tool results, reasoning,
/// and raw provider events.
fn echo_assistant_message(event: &AgentEvent) {
    if event.kind != AgentEventKind::AssistantMessage {
        return;
    }
    let Ok(message) = event.decode_payload::<AssistantMessage>() else {
        return;
    };
    echo_agent_lines(&message.text);
}

/// Writes one `agent:` line per non-blank line of `text`.
fn echo_agent_lines(text: &str) {
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        eprintln!("agent: {line}");
    }
}
