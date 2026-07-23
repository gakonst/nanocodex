use std::{process, time::Duration};

use eyre::{Result, WrapErr};
use nanocodex::{
    AgentEventKind, AgentEvents, Nanocodex, Responses, ResponsesBuilder, ResponsesTransport,
    StandardResponses, Thinking, Tools, TurnResult,
};
use tokio::task::JoinHandle;
use tower::{limit::ConcurrencyLimitLayer, timeout::TimeoutLayer};

const DEFAULT_WEBSOCKET_URL: &str = "wss://api.openai.com/v1/responses";
const DEFAULT_API_BASE_URL: &str = "https://api.openai.com/v1";

const LEDGER_PROMPT: &str = r"
You maintain an append-only release ledger using only facts explicitly supplied in this
conversation. Never infer entries that were added after the conversation checkpoint you can see.
Treat numbered ledger entries, mainline-only decisions, and branch-only decisions as distinct.
Do not use tools. Follow requested output formats exactly so humans can compare branches.
";

const LEDGER_ENTRIES: [&str; 10] = [
    "ORBIT: the product is named Atlas",
    "CEDAR: the database is PostgreSQL",
    "EMBER: the launch region is Dublin",
    "FJORD: deployments happen on Tuesday",
    "GLASS: the availability objective is 99.95 percent",
    "HARBOR: the worker queue capacity is 64",
    "ION: the status-page color is cobalt",
    "JUNIPER: the canary percentage is 5",
    "KITE: the incident captain is Mira",
    "LANTERN: the launch date is October 12",
];

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let api_key = std::env::var("OPENAI_API_KEY").wrap_err("OPENAI_API_KEY is required")?;

    // The fixed transport/storage policy and these layers are inherited by
    // every fork, while each fork receives fresh mutable transport state.
    let responses = configured_responses()?
        .layer(TimeoutLayer::new(Duration::from_secs(120)))
        .layer(ConcurrencyLimitLayer::new(1))
        .build();

    // A fully custom Service<ResponsesAttempt> is supplied as
    // `Responses::builder().service(|| make_service()).build()` so the root,
    // cancellation replacements, and every branch get independent mutable
    // service state.
    let tools = Tools::builder().without_defaults().build()?;
    let lineage = format!("fork-ledger-example-{}", process::id());
    let workspace = std::env::current_dir().wrap_err("failed to resolve the current directory")?;
    let (agent, root_events) = Nanocodex::builder(api_key)
        .session_id(&lineage)
        .instructions(LEDGER_PROMPT)
        .thinking(Thinking::Low)
        .tools(tools)
        .workspace(workspace)
        .responses(responses)
        .build()?;

    println!("conversation lineage/cache key: {lineage}");
    println!("building ten committed checkpoints on the root branch\n");
    let mut observers = vec![observe_events("root", root_events)];
    let mut checkpoints = Vec::with_capacity(LEDGER_ENTRIES.len());

    for (index, entry) in LEDGER_ENTRIES.iter().enumerate() {
        let turn = index + 1;
        let result = agent
            .prompt(format!(
                "Append numbered ledger entry {turn}: `{entry}`. Preserve every earlier entry. \
                 Reply exactly `STORED_{turn:02}`."
            ))
            .await?
            .result()
            .await?;
        println!("root {turn:02}: {}", result.final_message);
        checkpoints.push(result);
    }

    // `fork()` samples the latest safe model/tool boundary, which is
    // deterministically completed turn 10 here. The response ID remains private.
    let (latest, latest_events) = agent.fork().await?;
    observers.push(observe_events("latest@10", latest_events));

    // Prompt acceptance is separate from result waiting. The root driver starts
    // turn 11, but remains responsive to exact historical fork commands.
    let mainline_11 = agent
        .prompt(
            "Record the mainline-only decision `production_region=Helsinki`; it is not a numbered \
             ledger entry. Reply exactly: `MAINLINE count=10; latest=LANTERN; queue=64; \
             captain=Mira; launch_date=October 12; production=Helsinki`.",
        )
        .await?;
    println!("\nroot turn 11 accepted; forking turns 3, 6, and 9 while it runs");

    let ((branch_3, events_3), (branch_6, events_6), (branch_9, events_9)) = tokio::try_join!(
        agent.fork_from(checkpoint(&checkpoints, 3)?),
        agent.fork_from(checkpoint(&checkpoints, 6)?),
        agent.fork_from(checkpoint(&checkpoints, 9)?),
    )?;
    observers.extend([
        observe_events("branch@3", events_3),
        observe_events("branch@6", events_6),
        observe_events("branch@9", events_9),
    ]);

    // Every branch has a new driver, WebSocket, event stream, session ID, and
    // tool runtime. They share immutable local history and the lineage cache key.
    let branch_3_turn = branch_3.prompt(checkpoint_question(3, "EMBER")).await?;
    let branch_6_turn = branch_6.prompt(checkpoint_question(6, "HARBOR")).await?;
    let branch_9_turn = branch_9.prompt(checkpoint_question(9, "KITE")).await?;
    let latest_turn = latest.prompt(checkpoint_question(10, "LANTERN")).await?;

    let (mainline_11, result_3, result_6, result_9, result_10) = tokio::try_join!(
        mainline_11.result(),
        branch_3_turn.result(),
        branch_6_turn.result(),
        branch_9_turn.result(),
        latest_turn.result(),
    )?;
    println!("\ncheckpoint views (UNKNOWN proves later context did not leak backward)");
    println!("root mainline : {}", mainline_11.final_message);
    println!("branch from 03: {}", result_3.final_message);
    println!("branch from 06: {}", result_6.final_message);
    println!("branch from 09: {}", result_9.final_message);
    println!("latest at 10 : {}", result_10.final_message);

    // A branch is a normal Nanocodex handle: it retains its own response chain
    // and can diverge while the root continues independently.
    let branch_divergence = branch_3
        .prompt(
            "Record the branch-only decision `worker_queue_capacity=8`. Reply exactly: \
             `BRANCH_03 queue=8; production=UNKNOWN`.",
        )
        .await?;
    let root_continuation = agent
        .prompt(
            "Report the root's current decisions. Reply exactly: \
             `ROOT queue=64; production=Helsinki; branch_queue_override=UNKNOWN`.",
        )
        .await?;
    let (branch_divergence, root_continuation) =
        tokio::try_join!(branch_divergence.result(), root_continuation.result())?;
    println!("\nindependent continuation");
    println!("branch from 03: {}", branch_divergence.final_message);
    println!("root           : {}", root_continuation.final_message);

    // Dropping command handles stops their drivers and closes their independent
    // event streams. TurnResults may outlive the agents as inert checkpoints.
    drop((agent, latest, branch_3, branch_6, branch_9));
    for observer in observers {
        observer.await?;
    }
    Ok(())
}

fn checkpoint(results: &[TurnResult], turn: usize) -> Result<&TurnResult> {
    results
        .get(turn.saturating_sub(1))
        .ok_or_else(|| eyre::eyre!("missing completed turn {turn}"))
}

fn checkpoint_question(turn: usize, latest: &str) -> String {
    format!(
        "Inspect only the context available at this checkpoint. Reply exactly one line using \
         UNKNOWN for facts not yet introduced: `CHECKPOINT_{turn:02} count={turn}; latest={latest}; \
         queue=<value-or-UNKNOWN>; captain=<value-or-UNKNOWN>; launch_date=<value-or-UNKNOWN>; \
         production=<value-or-UNKNOWN>`."
    )
}

fn observe_events(label: &'static str, mut events: AgentEvents) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if matches!(
                event.kind,
                AgentEventKind::ModelConnectionCompleted
                    | AgentEventKind::ModelAttemptRetrying
                    | AgentEventKind::RunCompleted
                    | AgentEventKind::RunFailed
            ) {
                eprintln!(
                    "[{label}] event={} seq={} request_id={}",
                    event_kind(event.kind),
                    event.seq,
                    event.request_id
                );
            }
        }
    })
}

const fn event_kind(kind: AgentEventKind) -> &'static str {
    match kind {
        AgentEventKind::ModelConnectionCompleted => "model.connection.completed",
        AgentEventKind::ModelAttemptRetrying => "model.attempt.retrying",
        AgentEventKind::RunCompleted => "run.completed",
        AgentEventKind::RunFailed => "run.failed",
        _ => "other",
    }
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn configured_responses() -> Result<ResponsesBuilder<StandardResponses>> {
    let transport = std::env::var("NANOCODEX_RESPONSES_TRANSPORT")
        .unwrap_or_else(|_| ResponsesTransport::WebSocket.to_string())
        .parse::<ResponsesTransport>()
        .map_err(eyre::Report::msg)?;
    let mut responses = Responses::builder()
        .transport(transport)
        .websocket_url(env_or(
            "OPENAI_RESPONSES_WEBSOCKET_URL",
            DEFAULT_WEBSOCKET_URL,
        ))
        .api_base_url(env_or("OPENAI_API_BASE_URL", DEFAULT_API_BASE_URL));
    if let Ok(store) = std::env::var("NANOCODEX_STORE_RESPONSES") {
        responses = responses.store(
            store
                .parse::<bool>()
                .wrap_err("NANOCODEX_STORE_RESPONSES must be true or false")?,
        );
    }
    Ok(responses)
}
