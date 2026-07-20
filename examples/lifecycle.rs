use std::{process, time::Duration};

use eyre::{Result, WrapErr};
use nanocodex::{
    AgentEventKind, AgentEvents, Nanocodex, NanocodexError, Responses, Thinking, Tools,
};
use tokio::task::JoinHandle;
use tower::timeout::TimeoutLayer;

const INSTRUCTIONS: &str = r"
Maintain a small release ledger using only facts supplied in this conversation.
Never infer facts that are absent from your checkpoint. Do not use tools.
Keep every response to one concise line.
";

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let api_key = std::env::var("OPENAI_API_KEY").wrap_err("OPENAI_API_KEY is required")?;
    let workspace = std::env::current_dir().wrap_err("failed to resolve the workspace")?;

    // Layers wrap the standard persistent-WebSocket and retry service. A fully
    // custom stack uses `Responses::builder().service(|| make_stack())` so
    // cancellation, forks, and children always receive fresh mutable state.
    let responses = Responses::builder()
        .layer(TimeoutLayer::new(Duration::from_secs(120)))
        .build();

    // `tools(tools)` is the normal path for shareable handlers. Agent-relative
    // subagent tools instead use `tools_factory(|handle| ...)`; see subagents.rs.
    let tools = Tools::builder().without_defaults().build()?;
    let (agent, events) = Nanocodex::builder(api_key)
        .session_id(format!("lifecycle-example-{}", process::id()))
        .instructions(INSTRUCTIONS)
        .thinking(Thinking::Low)
        .workspace(workspace)
        .tools(tools)
        .responses(responses)
        .build()?;
    let mut observers = vec![observe("root", events)];

    // prompt().await means accepted, not completed. result().await consumes the
    // exact Turn and returns an immutable result/checkpoint.
    let historical_checkpoint = agent
        .prompt(
            "Record `codename=Atlas` and `database=PostgreSQL`. Reply exactly: CHECKPOINT_READY",
        )
        .await?
        .result()
        .await?;
    println!("checkpoint: {}", historical_checkpoint.final_message);

    // Turn is the direct control capability for unfinished work. TurnControl is
    // only needed when another task owns the single result receiver.
    let steered = agent
        .prompt("Recommend a deployment strategy for Atlas.")
        .await?;
    let control = steered.control();
    let result_task = tokio::spawn(async move { steered.result().await });
    control
        .steer("Also require zero-downtime database migrations.")
        .await?;
    let steered = result_task.await.wrap_err("steered result task failed")??;
    println!("steered: {}", steered.final_message);

    // Ordinary prompts are distinct FIFO turns. Cancellation targets the exact
    // accepted Turn whether it is still queued or has just become active.
    let active = agent
        .prompt("Compare blue-green and canary deployment in one sentence.")
        .await?;
    let cancelled = agent
        .prompt("Record `budget=9`; this queued turn will be cancelled.")
        .await?;
    cancelled.cancel().await?;
    let (active, cancelled) = tokio::join!(active.result(), cancelled.result());
    let active = active?;
    assert!(matches!(cancelled, Err(NanocodexError::TurnCancelled)));
    println!(
        "completed ahead of cancelled turn: {}",
        active.final_message
    );

    // Fork commands remain responsive while a root turn runs. fork() samples
    // the latest completed checkpoint; fork_from() selects an exact old result.
    let mainline = agent
        .prompt("Record the mainline-only fact `release=Tuesday`.")
        .await?;
    let ((historical, historical_events), (latest, latest_events)) =
        tokio::try_join!(agent.fork_from(&historical_checkpoint), agent.fork(),)?;
    observers.push(observe("historical", historical_events));
    observers.push(observe("latest", latest_events));

    let historical_turn = historical
        .prompt("Report codename, database, budget, and release. Use UNKNOWN for absent facts.");
    let latest_turn = latest
        .prompt("Report codename, database, budget, and release. Use UNKNOWN for absent facts.");
    let (historical_turn, latest_turn) = tokio::try_join!(historical_turn, latest_turn)?;
    let (mainline, historical_result, latest_result) = tokio::try_join!(
        mainline.result(),
        historical_turn.result(),
        latest_turn.result(),
    )?;

    println!("mainline:   {}", mainline.final_message);
    println!("historical: {}", historical_result.final_message);
    println!("latest:     {}", latest_result.final_message);

    // AgentEvents is independent from typed results. Dropping every command
    // handle stops its private driver and closes that agent's event stream.
    drop((agent, historical, latest));
    for observer in observers {
        observer.await.wrap_err("event observer failed")?;
    }
    Ok(())
}

fn observe(label: &'static str, mut events: AgentEvents) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if matches!(
                event.kind,
                AgentEventKind::RunStarted
                    | AgentEventKind::RunSteered
                    | AgentEventKind::RunCompleted
                    | AgentEventKind::RunFailed
            ) {
                eprintln!(
                    "[{label}] {:?} seq={} request_id={}",
                    event.kind, event.seq, event.request_id
                );
            }
        }
    })
}
