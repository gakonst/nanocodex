//! Live proof that stored Responses IDs form reusable historical checkpoints.
//!
//! The demo builds a ten-turn chain on one WebSocket, then continues that mainline while three
//! fresh `WebSocket` connections fork concurrently from the completed responses at turns three,
//! six, and nine.

use std::{
    fmt::Write as _,
    process,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use eyre::{Context, Result, bail, eyre};
use futures_util::{SinkExt, StreamExt, future::join_all};
use http::{HeaderValue, header};
use serde_json::{Value, json};
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

const DEFAULT_ENDPOINT: &str = "wss://api.openai.com/v1/responses";
const DEFAULT_HTTP_BASE: &str = "https://api.openai.com/v1";
const MODEL: &str = "gpt-5.6-sol";
const BETA: &str = "responses_websockets=2026-02-06";
const DEFAULT_TURNS: usize = 10;
const DEFAULT_PREFIX_FACTS: usize = 600;
const HISTORICAL_FORK_TURNS: [usize; 3] = [3, 6, 9];
const MAINLINE_CONTINUATIONS: usize = 2;
const IO_TIMEOUT: Duration = Duration::from_mins(2);

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Clone, Default)]
struct Usage {
    input: u64,
    cached: u64,
    cache_write: u64,
    output: u64,
}

struct ResponseRun {
    response_id: String,
    output: Vec<Value>,
    reply: String,
    usage: Usage,
    request_bytes: usize,
    latency: Duration,
}

struct ConnectedSocket {
    socket: Socket,
    connect_latency: Duration,
}

struct BranchRun {
    from_turn: usize,
    connect_latency: Duration,
    response: ResponseRun,
    full_replay_bytes: usize,
}

struct DemoConfig {
    endpoint: String,
    http_base: String,
    api_key: String,
    turns: usize,
    prefix_facts: usize,
    retain: bool,
}

#[derive(Clone)]
struct TurnCheckpoint {
    turn: usize,
    response_id: String,
    full_history: Vec<Value>,
}

struct LiveChain {
    connection: ConnectedSocket,
    root_session: String,
    head_response_id: String,
    full_history: Vec<Value>,
    checkpoints: Vec<TurnCheckpoint>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let config = DemoConfig::from_env()?;
    let mut stored_response_ids = Vec::new();
    let result = run_demo(&config, &mut stored_response_ids).await;

    if config.retain {
        println!(
            "\nretained {} stored responses because FORK_BENCH_RETAIN=1",
            stored_response_ids.len()
        );
    } else {
        cleanup_responses(&config, &stored_response_ids).await;
    }

    result
}

impl DemoConfig {
    fn from_env() -> Result<Self> {
        let turns = env_usize("FORK_BENCH_TURNS", DEFAULT_TURNS)?;
        if turns < *HISTORICAL_FORK_TURNS.last().unwrap_or(&0) {
            bail!(
                "FORK_BENCH_TURNS must be at least 9 for historical forks from turns 3, 6, and 9"
            );
        }
        Ok(Self {
            endpoint: std::env::var("OPENAI_RESPONSES_WEBSOCKET_URL")
                .unwrap_or_else(|_| DEFAULT_ENDPOINT.to_owned()),
            http_base: std::env::var("OPENAI_API_BASE_URL")
                .unwrap_or_else(|_| DEFAULT_HTTP_BASE.to_owned())
                .trim_end_matches('/')
                .to_owned(),
            api_key: std::env::var("OPENAI_API_KEY").wrap_err("OPENAI_API_KEY is required")?,
            turns,
            prefix_facts: env_usize("FORK_BENCH_PREFIX_FACTS", DEFAULT_PREFIX_FACTS)?,
            retain: std::env::var("FORK_BENCH_RETAIN").is_ok_and(|value| value == "1"),
        })
    }
}

async fn run_demo(config: &DemoConfig, stored_response_ids: &mut Vec<String>) -> Result<()> {
    let run_id = run_id()?;
    let cache_key = format!("nc-fork-{run_id}");
    println!("checkpoint fork benchmark");
    println!("model: {MODEL}");
    println!(
        "initial turns: {}, historical forks: {:?}, mainline continuations: {MAINLINE_CONTINUATIONS}",
        config.turns, HISTORICAL_FORK_TURNS
    );
    println!("prompt cache key: {cache_key}");
    println!("store: true");

    let chain = run_chain(config, &run_id, &cache_key, stored_response_ids).await?;
    let checkpoints = historical_checkpoints(&chain.checkpoints)?;
    let race_started = Instant::now();
    let (mainline, branches) = tokio::join!(
        continue_mainline(config, &cache_key, chain),
        run_historical_forks(config, &run_id, &cache_key, &checkpoints),
    );
    let mainline_ids = mainline?;
    let branches = branches?;
    stored_response_ids.extend(mainline_ids);
    stored_response_ids.extend(
        branches
            .iter()
            .map(|branch| branch.response.response_id.clone()),
    );
    println!(
        "mainline + historical forks wall time: {} ms",
        race_started.elapsed().as_millis()
    );
    print_summary(&branches);
    Ok(())
}

async fn run_chain(
    config: &DemoConfig,
    run_id: &str,
    cache_key: &str,
    stored_response_ids: &mut Vec<String>,
) -> Result<LiveChain> {
    let root_session = format!("nc-root-{run_id}");
    let developer = developer_message(config.prefix_facts);
    let mut full_history = vec![developer.clone()];
    let mut previous_response_id = None;
    let mut chain_usage = Usage::default();
    let mut checkpoints = Vec::with_capacity(config.turns);
    let mut connection = connect(config, &root_session).await?;
    println!(
        "root WebSocket connected in {} ms\n",
        connection.connect_latency.as_millis()
    );
    println!("chain (all turns use the same WebSocket)");
    println!("turn  request_B  latency_ms  input  cached  cache%  output  reply");

    for turn in 1..=config.turns {
        let user = user_message(&format!(
            "Conversation turn {turn}. Reply with exactly ACK_{turn:02}."
        ));
        let input = if turn == 1 {
            vec![developer.clone(), user.clone()]
        } else {
            vec![user.clone()]
        };
        let payload = response_request(
            cache_key,
            &root_session,
            &input,
            previous_response_id.as_deref(),
            true,
        );
        let response = send_response(&mut connection.socket, payload).await?;
        stored_response_ids.push(response.response_id.clone());
        print_response_row(turn, &response);

        chain_usage.add(&response.usage);
        previous_response_id = Some(response.response_id.clone());
        full_history.push(user);
        full_history.extend(response.output);
        checkpoints.push(TurnCheckpoint {
            turn,
            response_id: response.response_id,
            full_history: full_history.clone(),
        });
    }

    let head_response_id =
        previous_response_id.ok_or_else(|| eyre!("the conversation produced no checkpoint"))?;
    let full_history_bytes = serde_json::to_vec(&full_history)?.len();
    println!(
        "\ncurrent head after turn {}: {head_response_id}",
        config.turns
    );
    println!(
        "locally serialized checkpoint history: {} items / {full_history_bytes} bytes",
        full_history.len()
    );
    println!(
        "chain tokens: {} input / {} cached ({:.1}%) / {} output",
        chain_usage.input,
        chain_usage.cached,
        percentage(chain_usage.cached, chain_usage.input),
        chain_usage.output
    );
    for turn in HISTORICAL_FORK_TURNS {
        let checkpoint = checkpoints
            .iter()
            .find(|checkpoint| checkpoint.turn == turn)
            .ok_or_else(|| eyre!("missing checkpoint for turn {turn}"))?;
        println!(
            "retained turn-{turn} checkpoint: {}",
            checkpoint.response_id
        );
    }
    Ok(LiveChain {
        connection,
        root_session,
        head_response_id,
        full_history,
        checkpoints,
    })
}

fn historical_checkpoints(checkpoints: &[TurnCheckpoint]) -> Result<Vec<TurnCheckpoint>> {
    HISTORICAL_FORK_TURNS
        .iter()
        .map(|turn| {
            checkpoints
                .iter()
                .find(|checkpoint| checkpoint.turn == *turn)
                .cloned()
                .ok_or_else(|| eyre!("missing checkpoint for turn {turn}"))
        })
        .collect()
}

async fn continue_mainline(
    config: &DemoConfig,
    cache_key: &str,
    mut chain: LiveChain,
) -> Result<Vec<String>> {
    println!("\nmainline continues on the original WebSocket while forks run");
    println!("turn  request_B  latency_ms  input  cached  cache%  output  reply");
    let mut response_ids = Vec::with_capacity(MAINLINE_CONTINUATIONS);
    for turn in config.turns + 1..=config.turns + MAINLINE_CONTINUATIONS {
        let user = user_message(&format!(
            "Mainline conversation turn {turn}. Reply with exactly MAIN_{turn:02}."
        ));
        let payload = response_request(
            cache_key,
            &chain.root_session,
            std::slice::from_ref(&user),
            Some(&chain.head_response_id),
            true,
        );
        let response = send_response(&mut chain.connection.socket, payload).await?;
        print_response_row(turn, &response);
        chain.head_response_id.clone_from(&response.response_id);
        response_ids.push(response.response_id);
        chain.full_history.push(user);
        chain.full_history.extend(response.output);
    }
    Ok(response_ids)
}

async fn run_historical_forks(
    config: &DemoConfig,
    run_id: &str,
    cache_key: &str,
    checkpoints: &[TurnCheckpoint],
) -> Result<Vec<BranchRun>> {
    println!("\nhistorical forks on fresh WebSockets");
    println!("from  connect_ms  model_ms  delta_B  replay_B  saved%  input  cached  cache%  reply");
    let futures = checkpoints
        .iter()
        .map(|checkpoint| run_branch(config, run_id, cache_key, checkpoint));
    let branches: Vec<BranchRun> = join_all(futures).await.into_iter().collect::<Result<_>>()?;
    for branch in &branches {
        print_branch_row(branch);
    }
    Ok(branches)
}

fn print_summary(branches: &[BranchRun]) {
    let total_delta_bytes: usize = branches
        .iter()
        .map(|branch| branch.response.request_bytes)
        .sum();
    let total_replay_bytes: usize = branches.iter().map(|branch| branch.full_replay_bytes).sum();
    println!("\nsummary");
    println!(
        "three checkpoint forks sent {total_delta_bytes} request bytes; equivalent full replays would send {total_replay_bytes} bytes ({:.1}% saved)",
        100.0 - percentage(total_delta_bytes as u64, total_replay_bytes as u64)
    );
    println!(
        "checkpoint IDs avoid replay bytes, while usage still counts the complete logical context"
    );
}

async fn run_branch(
    config: &DemoConfig,
    run_id: &str,
    cache_key: &str,
    checkpoint: &TurnCheckpoint,
) -> Result<BranchRun> {
    let session_id = format!("nc-branch-from-{}-{run_id}", checkpoint.turn);
    let user = user_message(&format!(
        "You are a historical fork from turn {}. Reply exactly FORK_FROM_{:02}.",
        checkpoint.turn, checkpoint.turn
    ));
    let payload = response_request(
        cache_key,
        &session_id,
        std::slice::from_ref(&user),
        Some(&checkpoint.response_id),
        true,
    );
    let mut replay_input = checkpoint.full_history.clone();
    replay_input.push(user);
    let replay_payload = response_request(cache_key, &session_id, &replay_input, None, true);
    let full_replay_bytes = serde_json::to_vec(&replay_payload)?.len();
    let mut connection = connect(config, &session_id).await?;
    let response = send_response(&mut connection.socket, payload).await?;
    Ok(BranchRun {
        from_turn: checkpoint.turn,
        connect_latency: connection.connect_latency,
        response,
        full_replay_bytes,
    })
}

async fn connect(config: &DemoConfig, session_id: &str) -> Result<ConnectedSocket> {
    let started = Instant::now();
    let mut request = config
        .endpoint
        .as_str()
        .into_client_request()
        .wrap_err("invalid Responses WebSocket URL")?;
    request.headers_mut().insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", config.api_key))?,
    );
    request
        .headers_mut()
        .insert("OpenAI-Beta", HeaderValue::from_static(BETA));
    request.headers_mut().insert(
        "x-openai-internal-codex-responses-lite",
        HeaderValue::from_static("true"),
    );
    for name in ["session-id", "thread-id", "x-client-request-id"] {
        request
            .headers_mut()
            .insert(name, HeaderValue::from_str(session_id)?);
    }
    request.headers_mut().insert(
        header::USER_AGENT,
        HeaderValue::from_static("nanocodex-fork-checkpoint-bench/0.1"),
    );
    let (socket, _) = timeout(Duration::from_secs(20), connect_async(request))
        .await
        .wrap_err("Responses WebSocket handshake timed out")?
        .wrap_err("Responses WebSocket handshake failed")?;
    Ok(ConnectedSocket {
        socket,
        connect_latency: started.elapsed(),
    })
}

async fn send_response(socket: &mut Socket, payload: Value) -> Result<ResponseRun> {
    let encoded = serde_json::to_string(&payload)?;
    let request_bytes = encoded.len();
    let started = Instant::now();
    timeout(IO_TIMEOUT, socket.send(Message::Text(encoded.into())))
        .await
        .wrap_err("sending response.create timed out")?
        .wrap_err("sending response.create failed")?;

    let mut response_id = None;
    loop {
        let message = timeout(IO_TIMEOUT, socket.next())
            .await
            .wrap_err("waiting for a Responses event timed out")?
            .ok_or_else(|| eyre!("Responses WebSocket ended before a terminal event"))?
            .wrap_err("failed to receive a Responses event")?;
        match message {
            Message::Text(text) => {
                let event: Value = serde_json::from_str(text.as_str())?;
                let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");
                if let Some(id) = event
                    .get("response")
                    .and_then(|response| response.get("id"))
                    .and_then(Value::as_str)
                {
                    response_id = Some(id.to_owned());
                }
                match event_type {
                    "response.completed" => {
                        let response = event
                            .get("response")
                            .ok_or_else(|| eyre!("response.completed omitted response"))?;
                        let id = response
                            .get("id")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                            .or(response_id)
                            .ok_or_else(|| eyre!("completed response omitted its ID"))?;
                        let output = response
                            .get("output")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        return Ok(ResponseRun {
                            response_id: id,
                            reply: extract_reply(&output),
                            output,
                            usage: parse_usage(response.get("usage")),
                            request_bytes,
                            latency: started.elapsed(),
                        });
                    }
                    "response.failed" | "response.incomplete" | "error" => {
                        bail!("Responses terminal failure: {event}");
                    }
                    _ => {}
                }
            }
            Message::Close(frame) => bail!("Responses WebSocket closed early: {frame:?}"),
            Message::Binary(_) => bail!("Responses WebSocket returned binary data"),
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

fn response_request(
    cache_key: &str,
    session_id: &str,
    input: &[Value],
    previous_response_id: Option<&str>,
    store: bool,
) -> Value {
    let mut request = json!({
        "type": "response.create",
        "model": MODEL,
        "input": input,
        "tool_choice": "auto",
        "parallel_tool_calls": false,
        "reasoning": {"effort": "low", "context": "all_turns"},
        "store": store,
        "stream": true,
        "include": ["reasoning.encrypted_content"],
        "prompt_cache_key": cache_key,
        "text": {"verbosity": "low"},
        "client_metadata": {
            "session_id": session_id,
            "thread_id": session_id,
            "ws_request_header_x_openai_internal_codex_responses_lite": "true"
        }
    });
    if let Some(previous_response_id) = previous_response_id
        && let Some(object) = request.as_object_mut()
    {
        object.insert(
            "previous_response_id".to_owned(),
            Value::String(previous_response_id.to_owned()),
        );
    }
    request
}

fn developer_message(prefix_facts: usize) -> Value {
    let mut text = String::from(
        "You are a checkpoint-fork benchmark. Follow the final sentence of each user message and reply with only the requested token. The remaining text is deterministic cache material.\n",
    );
    for fact in 0..prefix_facts {
        let _ = write!(text, "cache_fact_{fact:04}=deterministic_value_{fact:04}; ");
    }
    json!({
        "type": "message",
        "role": "developer",
        "content": [{"type": "input_text", "text": text}]
    })
}

fn user_message(text: &str) -> Value {
    json!({
        "type": "message",
        "role": "user",
        "content": [{"type": "input_text", "text": text}]
    })
}

fn parse_usage(usage: Option<&Value>) -> Usage {
    let usage = usage.unwrap_or(&Value::Null);
    Usage {
        input: usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        cached: usage
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        cache_write: usage
            .pointer("/input_tokens_details/cache_write_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        output: usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
    }
}

impl Usage {
    const fn add(&mut self, other: &Self) {
        self.input += other.input;
        self.cached += other.cached;
        self.cache_write += other.cache_write;
        self.output += other.output;
    }
}

fn extract_reply(output: &[Value]) -> String {
    output
        .iter()
        .filter_map(Value::as_object)
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
        .filter_map(|item| item.get("content").and_then(Value::as_array))
        .flatten()
        .filter(|content| content.get("type").and_then(Value::as_str) == Some("output_text"))
        .filter_map(|content| content.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("")
}

fn print_response_row(turn: usize, response: &ResponseRun) {
    println!(
        "{turn:>4}  {:>9}  {:>10}  {:>5}  {:>6}  {:>5.1}  {:>6}  {}",
        response.request_bytes,
        response.latency.as_millis(),
        response.usage.input,
        response.usage.cached,
        percentage(response.usage.cached, response.usage.input),
        response.usage.output,
        one_line(&response.reply)
    );
}

fn print_branch_row(branch: &BranchRun) {
    println!(
        "{:>4}  {:>10}  {:>8}  {:>7}  {:>8}  {:>6.1}  {:>5}  {:>6}  {:>5.1}  {}",
        branch.from_turn,
        branch.connect_latency.as_millis(),
        branch.response.latency.as_millis(),
        branch.response.request_bytes,
        branch.full_replay_bytes,
        100.0
            - percentage(
                branch.response.request_bytes as u64,
                branch.full_replay_bytes as u64
            ),
        branch.response.usage.input,
        branch.response.usage.cached,
        percentage(branch.response.usage.cached, branch.response.usage.input),
        one_line(&branch.response.reply)
    );
}

#[allow(clippy::cast_precision_loss)]
fn percentage(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 * 100.0 / whole as f64
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn env_usize(name: &str, default: usize) -> Result<usize> {
    match std::env::var(name) {
        Ok(value) => {
            let value = value
                .parse::<usize>()
                .wrap_err_with(|| format!("{name} must be a positive integer"))?;
            if value == 0 {
                bail!("{name} must be greater than zero");
            }
            Ok(value)
        }
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(error).wrap_err_with(|| format!("failed to read {name}")),
    }
}

fn run_id() -> Result<String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .wrap_err("system clock is before the Unix epoch")?
        .as_nanos();
    Ok(format!("{nanos:x}-{:x}", process::id()))
}

async fn cleanup_responses(config: &DemoConfig, response_ids: &[String]) {
    if response_ids.is_empty() {
        return;
    }
    let client = reqwest::Client::new();
    let mut deleted = 0;
    for response_id in response_ids.iter().rev() {
        let result = client
            .delete(format!("{}/responses/{response_id}", config.http_base))
            .bearer_auth(&config.api_key)
            .send()
            .await;
        match result {
            Ok(response) if response.status().is_success() => deleted += 1,
            Ok(response) => eprintln!(
                "warning: failed to delete stored response {response_id}: HTTP {}",
                response.status()
            ),
            Err(error) => {
                eprintln!("warning: failed to delete stored response {response_id}: {error}");
            }
        }
    }
    println!(
        "\ncleanup: deleted {deleted}/{} stored responses",
        response_ids.len()
    );
}
