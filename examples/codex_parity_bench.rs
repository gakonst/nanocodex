use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Instant,
};

use eyre::{Result, WrapErr, bail, eyre};
use nanocodex::{
    AgentEvents, Nanocodex, OpenAi, Thinking, Tools, Turn, TurnResult,
    agent::{events::AgentEventKind, session::SessionId},
    oai::{MODEL, auth::load_chatgpt_auth, responses::Usage},
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
struct Workload {
    schema_version: u32,
    task: String,
    model: String,
    reasoning_effort: String,
    text_verbosity: String,
    base_instructions: String,
    fact_count: usize,
    chain_turns: usize,
    fork_turns: Vec<usize>,
    first_prompt_prefix: String,
    mainline_prompt: String,
    prompt_fnv1a64: String,
}

#[allow(clippy::struct_field_names)]
#[derive(Default, Serialize)]
struct UsageMeasurement {
    input_tokens: u64,
    cached_input_tokens: u64,
    cache_write_input_tokens: u64,
    output_tokens: u64,
    reasoning_output_tokens: u64,
    total_tokens: u64,
}

impl UsageMeasurement {
    fn add(&mut self, usage: &Usage) {
        self.input_tokens += usage.input_tokens;
        self.cached_input_tokens += usage
            .input_tokens_details
            .as_ref()
            .map_or(0, |details| details.cached_tokens);
        self.cache_write_input_tokens += usage
            .input_tokens_details
            .as_ref()
            .map_or(0, |details| details.cache_write_tokens);
        self.output_tokens += usage.output_tokens;
        self.reasoning_output_tokens += usage
            .output_tokens_details
            .as_ref()
            .map_or(0, |details| details.reasoning_tokens);
        self.total_tokens += usage.total_tokens;
    }
}

#[derive(Deserialize)]
struct ModelCallCompleted {
    duration_ns: u64,
    time_to_first_event_ns: u64,
    time_to_first_output_ns: Option<u64>,
    usage: Option<Usage>,
}

#[derive(Serialize)]
struct TurnMeasurement {
    latency_ms: f64,
    model_duration_ms: f64,
    time_to_first_event_ms: f64,
    time_to_first_output_ms: Option<f64>,
    final_message: String,
    usage: UsageMeasurement,
}

#[derive(Serialize)]
struct BenchmarkResult {
    implementation: &'static str,
    model: String,
    reasoning_effort: String,
    text_verbosity: String,
    source_commit: String,
    workspace: String,
    agents_md_fnv1a64: String,
    workload_fnv1a64: String,
    prompt_fnv1a64: String,
    agent_build_wall_ms: f64,
    chain: Vec<TurnMeasurement>,
    chain_median_latency_ms: f64,
    fork_api_wall_ms: f64,
    mainline: TurnMeasurement,
    branches: Vec<TurnMeasurement>,
}

struct Args {
    cwd: PathBuf,
    workload: PathBuf,
    source_commit: String,
    auth_file: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let args = parse_args()?;
    let openai = if let Some(auth_file) = &args.auth_file {
        OpenAi::new(
            load_chatgpt_auth(auth_file)
                .wrap_err_with(|| format!("failed to load {}", auth_file.display()))?,
        )?
    } else {
        let api_key = env::var("OPENAI_API_KEY").wrap_err(
            "OPENAI_API_KEY is required unless --auth-file selects shared ChatGPT auth",
        )?;
        OpenAi::new(api_key)?
    };
    let (workload_bytes, workload) = load_workload(&args.workload)?;
    let agents_md = fs::read(args.cwd.join("AGENTS.md"))
        .wrap_err_with(|| format!("failed to read AGENTS.md from {}", args.cwd.display()))?;
    let prompts = prompts(&workload);
    let prompt_fnv1a64 = digest_strings(&prompts);
    if prompt_fnv1a64 != workload.prompt_fnv1a64 {
        bail!("generated prompts do not match the workload digest");
    }
    let lineage = SessionId::new();
    let tools = Tools::builder().without_defaults().build()?;
    let agent_build_started = Instant::now();
    let (agent, mut root_events) = Nanocodex::builder(openai)
        .session_id(lineage)
        .instructions(workload.base_instructions.clone())
        .thinking(Thinking::Low)
        .workspace(&args.cwd)
        .tools(tools)
        .build()?;
    let agent_build_wall_ms = elapsed_ms(agent_build_started);

    let mut chain = Vec::with_capacity(workload.chain_turns);
    let mut checkpoints = Vec::with_capacity(workload.chain_turns);
    for (index, prompt) in prompts.iter().take(workload.chain_turns).enumerate() {
        let expected = expected_chain_output(&workload, index + 1)?;
        let (result, measurement) =
            measured_turn(&agent, &mut root_events, prompt, &expected).await?;
        checkpoints.push(result);
        chain.push(measurement);
    }

    let main_started = Instant::now();
    let main_turn = agent.prompt(workload.mainline_prompt.clone()).await?;
    let fork_started = Instant::now();
    let ((branch_3, mut events_3), (branch_6, mut events_6), (branch_9, mut events_9)) = tokio::try_join!(
        agent.fork_from(checkpoint(&checkpoints, workload.fork_turns[0])?),
        agent.fork_from(checkpoint(&checkpoints, workload.fork_turns[1])?),
        agent.fork_from(checkpoint(&checkpoints, workload.fork_turns[2])?),
    )?;
    let fork_api_wall_ms = elapsed_ms(fork_started);

    let branch_prompts = prompts
        .iter()
        .skip(workload.chain_turns + 1)
        .collect::<Vec<_>>();
    let ((turn_3, started_3), (turn_6, started_6), (turn_9, started_9)) = tokio::try_join!(
        start_prompt(&branch_3, branch_prompts[0]),
        start_prompt(&branch_6, branch_prompts[1]),
        start_prompt(&branch_9, branch_prompts[2]),
    )?;

    let (
        (main_result, main_latency),
        (result_3, latency_3),
        (result_6, latency_6),
        (result_9, latency_9),
    ) = tokio::try_join!(
        await_result(main_turn, main_started),
        await_result(turn_3, started_3),
        await_result(turn_6, started_6),
        await_result(turn_9, started_9),
    )?;
    let main_expected = expected_mainline_output(&workload)?;
    let branch_expected = workload
        .fork_turns
        .iter()
        .map(|turn| expected_branch_output(&workload, *turn))
        .collect::<Result<Vec<_>>>()?;
    let mainline =
        measurement_after_result(main_result, &mut root_events, main_latency, &main_expected)
            .await?;
    let branch_3 =
        measurement_after_result(result_3, &mut events_3, latency_3, &branch_expected[0]).await?;
    let branch_6 =
        measurement_after_result(result_6, &mut events_6, latency_6, &branch_expected[1]).await?;
    let branch_9 =
        measurement_after_result(result_9, &mut events_9, latency_9, &branch_expected[2]).await?;

    let mut chain_latencies = chain.iter().map(|turn| turn.latency_ms).collect::<Vec<_>>();
    chain_latencies.sort_by(f64::total_cmp);
    let result = BenchmarkResult {
        implementation: "nanocodex",
        model: workload.model,
        reasoning_effort: workload.reasoning_effort,
        text_verbosity: workload.text_verbosity,
        source_commit: args.source_commit,
        workspace: args.cwd.display().to_string(),
        agents_md_fnv1a64: fnv1a64(&agents_md),
        workload_fnv1a64: fnv1a64(&workload_bytes),
        prompt_fnv1a64,
        agent_build_wall_ms,
        chain_median_latency_ms: median(&chain_latencies),
        chain,
        fork_api_wall_ms,
        mainline,
        branches: vec![branch_3, branch_6, branch_9],
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn load_workload(path: &Path) -> Result<(Vec<u8>, Workload)> {
    let bytes = fs::read(path)
        .wrap_err_with(|| format!("failed to read workload at {}", path.display()))?;
    let workload = serde_json::from_slice(&bytes).wrap_err("failed to decode parity workload")?;
    validate_workload(&workload)?;
    Ok((bytes, workload))
}

async fn measured_turn(
    agent: &Nanocodex,
    events: &mut AgentEvents,
    prompt: &str,
    expected: &str,
) -> Result<(TurnResult, TurnMeasurement)> {
    let started = Instant::now();
    let result = agent.prompt(prompt).await?.result().await?;
    let measurement = finish_measurement(&result, events, elapsed_ms(started), expected).await?;
    Ok((result, measurement))
}

async fn measurement_after_result(
    result: TurnResult,
    events: &mut AgentEvents,
    latency_ms: f64,
    expected: &str,
) -> Result<TurnMeasurement> {
    finish_measurement(&result, events, latency_ms, expected).await
}

async fn start_prompt(agent: &Nanocodex, prompt: &str) -> Result<(Turn, Instant)> {
    let started = Instant::now();
    Ok((agent.prompt(prompt).await?, started))
}

async fn await_result(turn: Turn, started: Instant) -> Result<(TurnResult, f64)> {
    let result = turn.result().await?;
    Ok((result, elapsed_ms(started)))
}

async fn finish_measurement(
    result: &TurnResult,
    events: &mut AgentEvents,
    latency_ms: f64,
    expected: &str,
) -> Result<TurnMeasurement> {
    if result.final_message().trim() != expected {
        bail!(
            "unexpected response: expected {expected:?}, got {:?}",
            result.final_message()
        );
    }
    let calls = drain_turn(events).await?;
    let mut usage = UsageMeasurement::default();
    let mut model_duration_ns = 0;
    let mut time_to_first_event_ns = 0;
    let mut time_to_first_output_ns = None;
    for call in calls {
        model_duration_ns += call.duration_ns;
        time_to_first_event_ns += call.time_to_first_event_ns;
        if let Some(duration) = call.time_to_first_output_ns {
            time_to_first_output_ns =
                Some(time_to_first_output_ns.map_or(duration, |total: u64| total + duration));
        }
        if let Some(call_usage) = call.usage {
            usage.add(&call_usage);
        }
    }
    Ok(TurnMeasurement {
        latency_ms,
        model_duration_ms: nanos_ms(model_duration_ns),
        time_to_first_event_ms: nanos_ms(time_to_first_event_ns),
        time_to_first_output_ms: time_to_first_output_ns.map(nanos_ms),
        final_message: result.final_message().to_owned(),
        usage,
    })
}

async fn drain_turn(events: &mut AgentEvents) -> Result<Vec<ModelCallCompleted>> {
    let mut calls = Vec::new();
    while let Some(event) = events.recv().await {
        if event.kind == AgentEventKind::ModelCallCompleted {
            calls.push(event.decode_payload()?);
        }
        if event.kind == AgentEventKind::RunFailed {
            bail!("agent emitted run.failed: {}", event.payload.get());
        }
        if event.kind == AgentEventKind::RunCompleted {
            return Ok(calls);
        }
    }
    Err(eyre!("agent event stream closed before run.completed"))
}

fn prompts(workload: &Workload) -> Vec<String> {
    let mut first = String::from(&workload.first_prompt_prefix);
    for index in 0..workload.fact_count {
        first.push('\n');
        let fact = format!("FACT_{index:04}=VALUE_{index:04}_ABCDEFGHIJKLMNOPQRSTUVWXYZ");
        first.push_str(&fact);
    }
    match workload.task.as_str() {
        "transport_control" => first.push_str("\nReply only ACK_01."),
        "stateful_ledger" => {
            first.push_str("\nReport the current ledger using the required STATE format.");
        }
        _ => {}
    }

    let mut prompts = vec![first];
    match workload.task.as_str() {
        "transport_control" => prompts.extend(
            (2..=workload.chain_turns).map(|index| format!("Reply only ACK_{index:02}.")),
        ),
        "stateful_ledger" => prompts.extend((2..=workload.chain_turns).map(|index| {
            format!(
                "Append EVENT_{index:02}=ORBIT_{index:02}. Report the current ledger using the required STATE format."
            )
        })),
        _ => {}
    }
    prompts.push(workload.mainline_prompt.clone());
    match workload.task.as_str() {
        "transport_control" => prompts.extend(
            workload
                .fork_turns
                .iter()
                .map(|turn| format!("Forked from turn {turn}. Reply only FORK_{turn:02}.")),
        ),
        "stateful_ledger" => prompts.extend(workload.fork_turns.iter().map(|_| {
            String::from(
                "Without changing the ledger, report its current state using the required FORK format.",
            )
        })),
        _ => {}
    }
    prompts
}

fn ledger_state(prefix: &str, count: usize) -> String {
    let probe = |index| {
        if count >= index {
            format!("ORBIT_{index:02}")
        } else {
            String::from("UNKNOWN")
        }
    };
    format!(
        "{prefix} count={count:02} first=ORBIT_01 latest=ORBIT_{count:02} probe04={} probe07={} probe10={} anchor=VALUE_0420_ABCDEFGHIJKLMNOPQRSTUVWXYZ",
        probe(4),
        probe(7),
        probe(10)
    )
}

fn expected_chain_output(workload: &Workload, turn: usize) -> Result<String> {
    match workload.task.as_str() {
        "transport_control" => Ok(format!("ACK_{turn:02}")),
        "stateful_ledger" => Ok(ledger_state("STATE", turn)),
        task => bail!("unknown workload task {task:?}"),
    }
}

fn expected_mainline_output(workload: &Workload) -> Result<String> {
    match workload.task.as_str() {
        "transport_control" => Ok(String::from("MAIN_11")),
        "stateful_ledger" => Ok(ledger_state("MAIN", workload.chain_turns + 1)),
        task => bail!("unknown workload task {task:?}"),
    }
}

fn expected_branch_output(workload: &Workload, turn: usize) -> Result<String> {
    match workload.task.as_str() {
        "transport_control" => Ok(format!("FORK_{turn:02}")),
        "stateful_ledger" => Ok(ledger_state("FORK", turn)),
        task => bail!("unknown workload task {task:?}"),
    }
}

fn checkpoint(results: &[TurnResult], turn: usize) -> Result<&TurnResult> {
    results
        .get(turn.saturating_sub(1))
        .ok_or_else(|| eyre!("missing completed turn {turn}"))
}

fn parse_args() -> Result<Args> {
    let mut cwd = env::current_dir()?;
    let mut workload =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../benchmarks/codex_parity_workload.json");
    let mut source_commit = String::from("unknown");
    let mut auth_file = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--cwd" => cwd = PathBuf::from(args.next().ok_or_else(|| eyre!("--cwd needs a path"))?),
            "--workload" => {
                workload = PathBuf::from(
                    args.next()
                        .ok_or_else(|| eyre!("--workload needs a path"))?,
                );
            }
            "--source-commit" => {
                source_commit = args
                    .next()
                    .ok_or_else(|| eyre!("--source-commit needs a value"))?;
            }
            "--auth-file" => {
                auth_file = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| eyre!("--auth-file needs a path"))?,
                ));
            }
            _ => bail!("unknown argument {arg:?}"),
        }
    }
    Ok(Args {
        cwd: cwd
            .canonicalize()
            .wrap_err("failed to canonicalize --cwd")?,
        workload: workload
            .canonicalize()
            .wrap_err("failed to canonicalize --workload")?,
        source_commit,
        auth_file: auth_file
            .map(|path| {
                path.canonicalize()
                    .wrap_err("failed to canonicalize --auth-file")
            })
            .transpose()?,
    })
}

fn validate_workload(workload: &Workload) -> Result<()> {
    if workload.schema_version != 1
        || workload.model != MODEL
        || workload.reasoning_effort != "low"
        || workload.text_verbosity != "low"
        || workload.chain_turns != 10
        || workload.fork_turns != [3, 6, 9]
        || !matches!(
            workload.task.as_str(),
            "transport_control" | "stateful_ledger"
        )
    {
        bail!("workload is incompatible with this parity harness");
    }
    Ok(())
}

fn digest_strings(values: &[String]) -> String {
    let mut bytes = Vec::new();
    for value in values {
        bytes.extend_from_slice(value.as_bytes());
        bytes.push(0);
    }
    fnv1a64(&bytes)
}

fn fnv1a64(bytes: &[u8]) -> String {
    let mut digest = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        digest ^= u64::from(*byte);
        digest = digest.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{digest:016x}")
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1_000.0
}

fn nanos_ms(nanos: u64) -> f64 {
    std::time::Duration::from_nanos(nanos).as_secs_f64() * 1_000.0
}

const fn median(sorted: &[f64]) -> f64 {
    let middle = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        sorted[middle - 1].midpoint(sorted[middle])
    } else {
        sorted[middle]
    }
}
