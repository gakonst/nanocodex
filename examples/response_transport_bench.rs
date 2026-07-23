//! Live Responses transport, storage, history-replay, and fork benchmark.
//!
//! This is deliberately a direct API benchmark rather than an alternate
//! Nanocodex runtime. It holds the prompt and workload constant while varying
//! only transport, `store`, and whether prior context is referenced by response
//! ID or replayed by the client.

use std::{
    fmt::Write as _,
    path::PathBuf,
    process,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use eyre::{Context, Result, bail, eyre};
use futures_util::{SinkExt, StreamExt, future::join_all};
use http::{HeaderValue, header};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

const DEFAULT_WEBSOCKET_ENDPOINT: &str = "wss://api.openai.com/v1/responses";
const DEFAULT_HTTP_BASE: &str = "https://api.openai.com/v1";
const MODEL: &str = "gpt-5.6-sol";
const WEBSOCKET_BETA: &str = "responses_websockets=2026-02-06";
const DEFAULT_TURNS: usize = 4;
const DEFAULT_PREFIX_FACTS: usize = 600;
const DEFAULT_FORK_TURNS: &[usize] = &[2, 4];
const DEFAULT_MAINLINE_CONTINUATIONS: usize = 1;
const IO_TIMEOUT: Duration = Duration::from_secs(120);

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

const VARIANTS: &[Variant] = &[
    Variant {
        name: "ws-store-checkpoint",
        transport: Transport::WebSocket,
        store: true,
        chain_history: HistoryPolicy::PreviousResponseId,
        fork_history: HistoryPolicy::PreviousResponseId,
    },
    Variant {
        name: "ws-store-replay",
        transport: Transport::WebSocket,
        store: true,
        chain_history: HistoryPolicy::FullReplay,
        fork_history: HistoryPolicy::FullReplay,
    },
    Variant {
        name: "ws-ephemeral-connection",
        transport: Transport::WebSocket,
        store: false,
        chain_history: HistoryPolicy::PreviousResponseId,
        fork_history: HistoryPolicy::FullReplay,
    },
    Variant {
        name: "ws-ephemeral-replay",
        transport: Transport::WebSocket,
        store: false,
        chain_history: HistoryPolicy::FullReplay,
        fork_history: HistoryPolicy::FullReplay,
    },
    Variant {
        name: "https-store-checkpoint",
        transport: Transport::Https,
        store: true,
        chain_history: HistoryPolicy::PreviousResponseId,
        fork_history: HistoryPolicy::PreviousResponseId,
    },
    Variant {
        name: "https-store-replay",
        transport: Transport::Https,
        store: true,
        chain_history: HistoryPolicy::FullReplay,
        fork_history: HistoryPolicy::FullReplay,
    },
    Variant {
        name: "https-ephemeral-replay",
        transport: Transport::Https,
        store: false,
        chain_history: HistoryPolicy::FullReplay,
        fork_history: HistoryPolicy::FullReplay,
    },
];

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Transport {
    WebSocket,
    Https,
}

impl Transport {
    const fn as_str(self) -> &'static str {
        match self {
            Self::WebSocket => "websocket",
            Self::Https => "https",
        }
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum HistoryPolicy {
    PreviousResponseId,
    FullReplay,
}

impl HistoryPolicy {
    const fn as_str(self) -> &'static str {
        match self {
            Self::PreviousResponseId => "previous_response_id",
            Self::FullReplay => "full_replay",
        }
    }
}

#[derive(Clone, Copy)]
struct Variant {
    name: &'static str,
    transport: Transport,
    store: bool,
    chain_history: HistoryPolicy,
    fork_history: HistoryPolicy,
}

#[derive(Clone, Default, Serialize)]
struct Usage {
    input: u64,
    cached: u64,
    cache_write: u64,
    output: u64,
}

impl Usage {
    fn add(&mut self, other: &Self) {
        self.input += other.input;
        self.cached += other.cached;
        self.cache_write += other.cache_write;
        self.output += other.output;
    }
}

struct ResponseRun {
    response_id: String,
    output: Arc<[Arc<Value>]>,
    reply: String,
    usage: Usage,
    request_bytes: usize,
    encode_latency: Duration,
    response_latency: Duration,
    time_to_first_event: Duration,
}

impl ResponseRun {
    fn measurement(&self) -> ResponseMeasurement {
        ResponseMeasurement {
            response_id: self.response_id.clone(),
            reply: self.reply.clone(),
            request_bytes: self.request_bytes,
            encode_us: duration_us(self.encode_latency),
            response_ms: duration_ms(self.response_latency),
            time_to_first_event_ms: duration_ms(self.time_to_first_event),
            usage: self.usage.clone(),
        }
    }
}

#[derive(Serialize)]
struct ResponseMeasurement {
    response_id: String,
    reply: String,
    request_bytes: usize,
    encode_us: f64,
    response_ms: f64,
    time_to_first_event_ms: f64,
    usage: Usage,
}

#[derive(Serialize)]
struct BranchMeasurement {
    from_turn: usize,
    setup_ms: f64,
    response: ResponseMeasurement,
}

#[derive(Serialize)]
struct VariantMeasurement {
    variant: &'static str,
    transport: Transport,
    store: bool,
    chain_history: HistoryPolicy,
    fork_history: HistoryPolicy,
    root_setup_ms: f64,
    chain_wall_ms: f64,
    chain_median_response_ms: f64,
    chain_request_bytes: usize,
    chain_usage: Usage,
    chain: Vec<ResponseMeasurement>,
    fork_snapshot_clone_us: f64,
    mainline_and_forks_wall_ms: f64,
    mainline: Vec<ResponseMeasurement>,
    branches: Vec<BranchMeasurement>,
}

#[derive(Serialize)]
struct FailedVariant {
    variant: &'static str,
    error: String,
}

#[derive(Serialize)]
struct BenchmarkReport {
    schema_version: u32,
    model: &'static str,
    run_id: String,
    turns: usize,
    fork_turns: Vec<usize>,
    mainline_continuations: usize,
    prefix_facts: usize,
    repeats: usize,
    measurements: Vec<VariantMeasurement>,
    failures: Vec<FailedVariant>,
}

struct BenchConfig {
    websocket_endpoint: String,
    http_endpoint: String,
    api_key: String,
    turns: usize,
    prefix_facts: usize,
    fork_turns: Vec<usize>,
    mainline_continuations: usize,
    repeats: usize,
    variants: Vec<Variant>,
    output: Option<PathBuf>,
    retain: bool,
}

#[derive(Clone)]
struct History {
    head: Arc<HistorySegment>,
}

struct HistorySegment {
    previous: Option<Arc<HistorySegment>>,
    items: Arc<[Arc<Value>]>,
    len: usize,
}

impl History {
    fn new(item: Value) -> Self {
        Self {
            head: Arc::new(HistorySegment {
                previous: None,
                items: Arc::from([Arc::new(item)]),
                len: 1,
            }),
        }
    }

    fn append(&self, user: Arc<Value>, output: &Arc<[Arc<Value>]>) -> Self {
        let mut items = Vec::with_capacity(output.len() + 1);
        items.push(user);
        items.extend(output.iter().cloned());
        Self {
            head: Arc::new(HistorySegment {
                previous: Some(Arc::clone(&self.head)),
                len: self.head.len + items.len(),
                items: items.into(),
            }),
        }
    }

    fn refs(&self) -> Vec<&Value> {
        let mut segments = Vec::new();
        let mut current = Some(self.head.as_ref());
        while let Some(segment) = current {
            segments.push(segment);
            current = segment.previous.as_deref();
        }
        let mut items = Vec::with_capacity(self.head.len);
        for segment in segments.into_iter().rev() {
            items.extend(segment.items.iter().map(AsRef::as_ref));
        }
        items
    }
}

#[derive(Clone)]
struct TurnCheckpoint {
    turn: usize,
    response_id: String,
    history: History,
}

enum TransportClient {
    WebSocket(Box<Socket>),
    Https(reqwest::Client),
}

struct ConnectedTransport {
    client: TransportClient,
    setup_latency: Duration,
}

struct LiveChain {
    connection: ConnectedTransport,
    root_session: String,
    head_response_id: String,
    history: History,
    checkpoints: Vec<TurnCheckpoint>,
    responses: Vec<ResponseRun>,
}

struct BranchRun {
    from_turn: usize,
    setup_latency: Duration,
    response: ResponseRun,
}

struct MainlineRun {
    responses: Vec<ResponseRun>,
    stored_response_ids: Vec<String>,
}

#[derive(Serialize)]
struct RequestBody<'a> {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    kind: Option<&'static str>,
    model: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_response_id: Option<&'a str>,
    input: &'a [&'a Value],
    tool_choice: &'static str,
    parallel_tool_calls: bool,
    reasoning: Reasoning,
    store: bool,
    stream: bool,
    include: [&'static str; 1],
    prompt_cache_key: &'a str,
    text: TextControls,
    client_metadata: ClientMetadata<'a>,
}

#[derive(Serialize)]
struct Reasoning {
    effort: &'static str,
    context: &'static str,
}

#[derive(Serialize)]
struct TextControls {
    verbosity: &'static str,
}

#[derive(Serialize)]
struct ClientMetadata<'a> {
    session_id: &'a str,
    thread_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    ws_request_header_x_openai_internal_codex_responses_lite: Option<&'static str>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = rustls::crypto::ring::default_provider().install_default();

    let config = BenchConfig::from_env()?;
    let run_id = run_id()?;
    print_header(&config, &run_id);

    let mut measurements = Vec::with_capacity(config.variants.len() * config.repeats);
    let mut failures = Vec::new();
    let mut stored_response_ids = Vec::new();
    for repeat in 0..config.repeats {
        for offset in 0..config.variants.len() {
            let variant = config.variants[(offset + repeat) % config.variants.len()];
            let trial_id = format!("{run_id}-r{}-{}", repeat + 1, variant.name);
            match run_variant(&config, variant, &trial_id, &mut stored_response_ids).await {
                Ok(measurement) => {
                    print_variant_summary(&measurement);
                    measurements.push(measurement);
                }
                Err(error) => {
                    eprintln!("\n{} failed: {error:#}", variant.name);
                    failures.push(FailedVariant {
                        variant: variant.name,
                        error: format!("{error:#}"),
                    });
                }
            }
        }
    }

    let report = BenchmarkReport {
        schema_version: 1,
        model: MODEL,
        run_id,
        turns: config.turns,
        fork_turns: config.fork_turns.clone(),
        mainline_continuations: config.mainline_continuations,
        prefix_facts: config.prefix_facts,
        repeats: config.repeats,
        measurements,
        failures,
    };
    if let Some(path) = &config.output {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .wrap_err_with(|| format!("failed to create {}", parent.display()))?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(&report)?)
            .wrap_err_with(|| format!("failed to write {}", path.display()))?;
        println!("\nJSON report: {}", path.display());
    }

    if config.retain {
        println!(
            "retained {} stored responses because FORK_BENCH_RETAIN=1",
            stored_response_ids.len()
        );
    } else {
        cleanup_responses(&config, &stored_response_ids).await;
    }

    if report.failures.is_empty() {
        Ok(())
    } else {
        bail!("{} benchmark variant(s) failed", report.failures.len())
    }
}

impl BenchConfig {
    fn from_env() -> Result<Self> {
        let turns = env_usize("FORK_BENCH_TURNS", DEFAULT_TURNS)?;
        let fork_turns = env_usize_list("FORK_BENCH_FORK_TURNS", DEFAULT_FORK_TURNS)?;
        if let Some(turn) = fork_turns.iter().find(|turn| **turn > turns) {
            bail!("fork turn {turn} exceeds FORK_BENCH_TURNS={turns}");
        }
        let variants = selected_variants()?;
        Ok(Self {
            websocket_endpoint: std::env::var("OPENAI_RESPONSES_WEBSOCKET_URL")
                .unwrap_or_else(|_| DEFAULT_WEBSOCKET_ENDPOINT.to_owned()),
            http_endpoint: format!(
                "{}/responses",
                std::env::var("OPENAI_API_BASE_URL")
                    .unwrap_or_else(|_| DEFAULT_HTTP_BASE.to_owned())
                    .trim_end_matches('/')
            ),
            api_key: std::env::var("OPENAI_API_KEY").wrap_err("OPENAI_API_KEY is required")?,
            turns,
            prefix_facts: env_usize("FORK_BENCH_PREFIX_FACTS", DEFAULT_PREFIX_FACTS)?,
            fork_turns,
            mainline_continuations: env_usize(
                "FORK_BENCH_MAINLINE_CONTINUATIONS",
                DEFAULT_MAINLINE_CONTINUATIONS,
            )?,
            repeats: env_usize("FORK_BENCH_REPEATS", 1)?,
            variants,
            output: std::env::var_os("FORK_BENCH_OUTPUT").map(PathBuf::from),
            retain: std::env::var("FORK_BENCH_RETAIN").is_ok_and(|value| value == "1"),
        })
    }
}

async fn run_variant(
    config: &BenchConfig,
    variant: Variant,
    trial_id: &str,
    stored_response_ids: &mut Vec<String>,
) -> Result<VariantMeasurement> {
    println!(
        "\n{}: transport={} store={} chain={} forks={}",
        variant.name,
        variant.transport.as_str(),
        variant.store,
        variant.chain_history.as_str(),
        variant.fork_history.as_str()
    );
    let cache_key = format!("nc-transport-{trial_id}");
    let root_session = format!("nc-root-{trial_id}");
    let chain_started = Instant::now();
    let mut chain = run_chain(
        config,
        variant,
        &root_session,
        &cache_key,
        stored_response_ids,
    )
    .await?;
    let chain_wall = chain_started.elapsed();

    let snapshot_started = Instant::now();
    let checkpoints = select_checkpoints(&chain.checkpoints, &config.fork_turns)?;
    let snapshot_clone = snapshot_started.elapsed();

    let root_http_client = match &chain.connection.client {
        TransportClient::Https(client) => Some(client.clone()),
        TransportClient::WebSocket(_) => None,
    };
    let race_started = Instant::now();
    let branch_futures = checkpoints
        .iter()
        .map(|checkpoint| {
            run_branch(
                config,
                variant,
                trial_id,
                &cache_key,
                checkpoint,
                root_http_client.clone(),
            )
        })
        .collect::<Vec<_>>();
    let (mainline, branches) = tokio::join!(
        continue_mainline(config, variant, &cache_key, &mut chain),
        async {
            join_all(branch_futures)
                .await
                .into_iter()
                .collect::<Result<Vec<_>>>()
        }
    );
    let race_wall = race_started.elapsed();
    let mut mainline = mainline?;
    let branches = branches?;
    stored_response_ids.append(&mut mainline.stored_response_ids);
    if variant.store {
        stored_response_ids.extend(
            branches
                .iter()
                .map(|branch| branch.response.response_id.clone()),
        );
    }

    build_variant_measurement(
        variant,
        &chain,
        chain_wall,
        snapshot_clone,
        race_wall,
        &mainline,
        &branches,
    )
}

fn build_variant_measurement(
    variant: Variant,
    chain: &LiveChain,
    chain_wall: Duration,
    snapshot_clone: Duration,
    race_wall: Duration,
    mainline: &MainlineRun,
    branches: &[BranchRun],
) -> Result<VariantMeasurement> {
    let chain_responses = chain_measurements(chain)?;
    let mut chain_latencies = chain_responses
        .iter()
        .map(|response| response.response_ms)
        .collect::<Vec<_>>();
    chain_latencies.sort_by(f64::total_cmp);
    let chain_request_bytes = chain_responses
        .iter()
        .map(|response| response.request_bytes)
        .sum();
    let mut chain_usage = Usage::default();
    for response in &chain_responses {
        chain_usage.add(&response.usage);
    }
    Ok(VariantMeasurement {
        variant: variant.name,
        transport: variant.transport,
        store: variant.store,
        chain_history: variant.chain_history,
        fork_history: variant.fork_history,
        root_setup_ms: duration_ms(chain.connection.setup_latency),
        chain_wall_ms: duration_ms(chain_wall),
        chain_median_response_ms: median(&chain_latencies),
        chain_request_bytes,
        chain_usage,
        chain: chain_responses,
        fork_snapshot_clone_us: duration_us(snapshot_clone),
        mainline_and_forks_wall_ms: duration_ms(race_wall),
        mainline: mainline
            .responses
            .iter()
            .map(ResponseRun::measurement)
            .collect(),
        branches: branches
            .iter()
            .map(|branch| BranchMeasurement {
                from_turn: branch.from_turn,
                setup_ms: duration_ms(branch.setup_latency),
                response: branch.response.measurement(),
            })
            .collect(),
    })
}

async fn run_chain(
    config: &BenchConfig,
    variant: Variant,
    root_session: &str,
    cache_key: &str,
    stored_response_ids: &mut Vec<String>,
) -> Result<LiveChain> {
    let developer = developer_message(config.prefix_facts);
    let mut history = History::new(developer);
    let mut previous_response_id = None;
    let mut checkpoints = Vec::with_capacity(config.turns);
    let mut connection = connect_transport(config, variant.transport, root_session, None).await?;
    let mut responses = Vec::with_capacity(config.turns);

    for turn in 1..=config.turns {
        let user = Arc::new(user_message(&format!(
            "Conversation turn {turn}. Reply with exactly ACK_{turn:02}."
        )));
        let input = request_input(
            variant.chain_history,
            &history,
            &user,
            previous_response_id.is_some(),
        );
        let prior = matches!(variant.chain_history, HistoryPolicy::PreviousResponseId)
            .then_some(previous_response_id.as_deref())
            .flatten();
        let response = send_request(
            config,
            variant,
            &mut connection.client,
            cache_key,
            root_session,
            &input,
            prior,
        )
        .await?;
        require_reply(&response, &format!("ACK_{turn:02}"))?;
        if variant.store {
            stored_response_ids.push(response.response_id.clone());
        }
        previous_response_id = Some(response.response_id.clone());
        history = history.append(user, &response.output);
        checkpoints.push(TurnCheckpoint {
            turn,
            response_id: response.response_id.clone(),
            history: history.clone(),
        });
        responses.push(response);
    }

    let head_response_id =
        previous_response_id.ok_or_else(|| eyre!("the chain produced no response"))?;
    Ok(LiveChain {
        connection,
        root_session: root_session.to_owned(),
        head_response_id,
        history,
        checkpoints,
        responses,
    })
}

fn chain_measurements(chain: &LiveChain) -> Result<Vec<ResponseMeasurement>> {
    if chain.responses.len() != chain.checkpoints.len() {
        bail!(
            "chain measurement count {} did not match checkpoint count {}",
            chain.responses.len(),
            chain.checkpoints.len()
        );
    }
    Ok(chain
        .responses
        .iter()
        .map(ResponseRun::measurement)
        .collect())
}

async fn continue_mainline(
    config: &BenchConfig,
    variant: Variant,
    cache_key: &str,
    chain: &mut LiveChain,
) -> Result<MainlineRun> {
    let mut responses = Vec::with_capacity(config.mainline_continuations);
    let mut stored_response_ids = Vec::with_capacity(config.mainline_continuations);
    for continuation in 1..=config.mainline_continuations {
        let turn = config.turns + continuation;
        let user = Arc::new(user_message(&format!(
            "Mainline conversation turn {turn}. Reply with exactly MAIN_{turn:02}."
        )));
        let input = request_input(variant.chain_history, &chain.history, &user, true);
        let prior = matches!(variant.chain_history, HistoryPolicy::PreviousResponseId)
            .then_some(chain.head_response_id.as_str());
        let response = send_request(
            config,
            variant,
            &mut chain.connection.client,
            cache_key,
            &chain.root_session,
            &input,
            prior,
        )
        .await?;
        require_reply(&response, &format!("MAIN_{turn:02}"))?;
        chain.head_response_id.clone_from(&response.response_id);
        chain.history = chain.history.append(user, &response.output);
        if variant.store {
            stored_response_ids.push(response.response_id.clone());
        }
        responses.push(response);
    }
    Ok(MainlineRun {
        responses,
        stored_response_ids,
    })
}

async fn run_branch(
    config: &BenchConfig,
    variant: Variant,
    trial_id: &str,
    cache_key: &str,
    checkpoint: &TurnCheckpoint,
    http_client: Option<reqwest::Client>,
) -> Result<BranchRun> {
    let session_id = format!("nc-fork-{}-{trial_id}", checkpoint.turn);
    let user = Arc::new(user_message(&format!(
        "You are a historical fork from turn {}. Reply exactly FORK_FROM_{:02}.",
        checkpoint.turn, checkpoint.turn
    )));
    let input = request_input(variant.fork_history, &checkpoint.history, &user, true);
    let prior = matches!(variant.fork_history, HistoryPolicy::PreviousResponseId)
        .then_some(checkpoint.response_id.as_str());
    let mut connection =
        connect_transport(config, variant.transport, &session_id, http_client).await?;
    let response = send_request(
        config,
        variant,
        &mut connection.client,
        cache_key,
        &session_id,
        &input,
        prior,
    )
    .await?;
    require_reply(&response, &format!("FORK_FROM_{:02}", checkpoint.turn))?;
    Ok(BranchRun {
        from_turn: checkpoint.turn,
        setup_latency: connection.setup_latency,
        response,
    })
}

fn request_input<'a>(
    policy: HistoryPolicy,
    history: &'a History,
    user: &'a Arc<Value>,
    has_previous_response: bool,
) -> Vec<&'a Value> {
    if matches!(policy, HistoryPolicy::PreviousResponseId) && has_previous_response {
        vec![user.as_ref()]
    } else {
        let mut input = history.refs();
        input.push(user.as_ref());
        input
    }
}

async fn connect_transport(
    config: &BenchConfig,
    transport: Transport,
    session_id: &str,
    http_client: Option<reqwest::Client>,
) -> Result<ConnectedTransport> {
    let started = Instant::now();
    let client = match transport {
        Transport::WebSocket => {
            TransportClient::WebSocket(Box::new(connect_websocket(config, session_id).await?))
        }
        Transport::Https => {
            let client = match http_client {
                Some(client) => client,
                None => reqwest::Client::builder()
                    .timeout(IO_TIMEOUT)
                    .user_agent("nanocodex-response-transport-bench/0.1")
                    .build()
                    .wrap_err("failed to build HTTPS client")?,
            };
            TransportClient::Https(client)
        }
    };
    Ok(ConnectedTransport {
        client,
        setup_latency: started.elapsed(),
    })
}

async fn connect_websocket(config: &BenchConfig, session_id: &str) -> Result<Socket> {
    let mut request = config
        .websocket_endpoint
        .as_str()
        .into_client_request()
        .wrap_err("invalid Responses WebSocket URL")?;
    request.headers_mut().insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", config.api_key))?,
    );
    request
        .headers_mut()
        .insert("OpenAI-Beta", HeaderValue::from_static(WEBSOCKET_BETA));
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
        HeaderValue::from_static("nanocodex-response-transport-bench/0.1"),
    );
    let (socket, _) = timeout(Duration::from_secs(20), connect_async(request))
        .await
        .wrap_err("Responses WebSocket handshake timed out")?
        .wrap_err("Responses WebSocket handshake failed")?;
    Ok(socket)
}

async fn send_request(
    config: &BenchConfig,
    variant: Variant,
    transport: &mut TransportClient,
    cache_key: &str,
    session_id: &str,
    input: &[&Value],
    previous_response_id: Option<&str>,
) -> Result<ResponseRun> {
    let encode_started = Instant::now();
    let body = RequestBody {
        kind: matches!(variant.transport, Transport::WebSocket).then_some("response.create"),
        model: MODEL,
        previous_response_id,
        input,
        tool_choice: "auto",
        parallel_tool_calls: false,
        reasoning: Reasoning {
            effort: "low",
            context: "all_turns",
        },
        store: variant.store,
        stream: true,
        include: ["reasoning.encrypted_content"],
        prompt_cache_key: cache_key,
        text: TextControls { verbosity: "low" },
        client_metadata: ClientMetadata {
            session_id,
            thread_id: session_id,
            ws_request_header_x_openai_internal_codex_responses_lite: matches!(
                variant.transport,
                Transport::WebSocket
            )
            .then_some("true"),
        },
    };
    let encoded = serde_json::to_string(&body)?;
    let encode_latency = encode_started.elapsed();
    let request_bytes = encoded.len();
    match transport {
        TransportClient::WebSocket(socket) => {
            send_websocket(socket, encoded, request_bytes, encode_latency).await
        }
        TransportClient::Https(client) => {
            send_https(
                config,
                client,
                session_id,
                encoded,
                request_bytes,
                encode_latency,
            )
            .await
        }
    }
}

async fn send_websocket(
    socket: &mut Socket,
    encoded: String,
    request_bytes: usize,
    encode_latency: Duration,
) -> Result<ResponseRun> {
    let started = Instant::now();
    timeout(IO_TIMEOUT, socket.send(Message::Text(encoded.into())))
        .await
        .wrap_err("sending response.create timed out")?
        .wrap_err("sending response.create failed")?;

    let mut first_event = None;
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
                first_event.get_or_insert_with(|| started.elapsed());
                if let Some(completed) = process_event(&event, &mut response_id)? {
                    return response_run(
                        completed,
                        response_id,
                        request_bytes,
                        encode_latency,
                        started.elapsed(),
                        first_event.unwrap_or_else(|| started.elapsed()),
                    );
                }
            }
            Message::Close(frame) => bail!("Responses WebSocket closed early: {frame:?}"),
            Message::Binary(_) => bail!("Responses WebSocket returned binary data"),
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

async fn send_https(
    config: &BenchConfig,
    client: &reqwest::Client,
    session_id: &str,
    encoded: String,
    request_bytes: usize,
    encode_latency: Duration,
) -> Result<ResponseRun> {
    let started = Instant::now();
    let mut response = client
        .post(&config.http_endpoint)
        .bearer_auth(&config.api_key)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "text/event-stream")
        .header("x-openai-internal-codex-responses-lite", "true")
        .header("session-id", session_id)
        .header("thread-id", session_id)
        .header("x-client-request-id", session_id)
        .body(encoded)
        .send()
        .await
        .wrap_err("HTTPS Responses request failed")?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("HTTPS Responses request returned {status}: {body}");
    }

    let mut buffered = Vec::new();
    let mut first_event = None;
    let mut response_id = None;
    loop {
        let chunk = timeout(IO_TIMEOUT, response.chunk())
            .await
            .wrap_err("waiting for an HTTPS Responses event timed out")?
            .wrap_err("failed to read HTTPS Responses stream")?;
        let Some(chunk) = chunk else {
            bail!("HTTPS Responses stream ended before a terminal event");
        };
        buffered.extend_from_slice(&chunk);
        while let Some(newline) = buffered.iter().position(|byte| *byte == b'\n') {
            let mut line = buffered.drain(..=newline).collect::<Vec<_>>();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = std::str::from_utf8(&line).wrap_err("SSE line was not UTF-8")?;
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim_start();
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            let event: Value =
                serde_json::from_str(data).wrap_err("failed to decode HTTPS SSE event")?;
            first_event.get_or_insert_with(|| started.elapsed());
            if let Some(completed) = process_event(&event, &mut response_id)? {
                return response_run(
                    completed,
                    response_id,
                    request_bytes,
                    encode_latency,
                    started.elapsed(),
                    first_event.unwrap_or_else(|| started.elapsed()),
                );
            }
        }
    }
}

fn process_event<'a>(
    event: &'a Value,
    response_id: &mut Option<String>,
) -> Result<Option<&'a Value>> {
    if let Some(id) = event
        .get("response")
        .and_then(|response| response.get("id"))
        .and_then(Value::as_str)
    {
        *response_id = Some(id.to_owned());
    }
    match event.get("type").and_then(Value::as_str).unwrap_or("") {
        "response.completed" => event
            .get("response")
            .map(Some)
            .ok_or_else(|| eyre!("response.completed omitted response")),
        "response.failed" | "response.incomplete" | "error" => {
            bail!("Responses terminal failure: {event}")
        }
        _ => Ok(None),
    }
}

fn response_run(
    completed: &Value,
    observed_response_id: Option<String>,
    request_bytes: usize,
    encode_latency: Duration,
    response_latency: Duration,
    time_to_first_event: Duration,
) -> Result<ResponseRun> {
    let response_id = completed
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or(observed_response_id)
        .ok_or_else(|| eyre!("completed response omitted its ID"))?;
    let output = completed
        .get("output")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(Arc::new)
        .collect::<Vec<_>>();
    Ok(ResponseRun {
        response_id,
        reply: extract_reply(&output),
        output: output.into(),
        usage: parse_usage(completed.get("usage")),
        request_bytes,
        encode_latency,
        response_latency,
        time_to_first_event,
    })
}

fn require_reply(response: &ResponseRun, expected: &str) -> Result<()> {
    if response.reply.trim() == expected {
        Ok(())
    } else {
        bail!(
            "unexpected response: expected {expected:?}, got {:?}",
            response.reply
        )
    }
}

fn select_checkpoints(
    checkpoints: &[TurnCheckpoint],
    fork_turns: &[usize],
) -> Result<Vec<TurnCheckpoint>> {
    fork_turns
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

fn developer_message(prefix_facts: usize) -> Value {
    let mut text = String::from(
        "You are a Responses transport benchmark. Follow the final sentence of each user message and reply with only the requested token. The remaining text is deterministic cache material.\n",
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

fn extract_reply(output: &[Arc<Value>]) -> String {
    output
        .iter()
        .filter_map(|item| item.as_object())
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
        .filter_map(|item| item.get("content").and_then(Value::as_array))
        .flatten()
        .filter(|content| content.get("type").and_then(Value::as_str) == Some("output_text"))
        .filter_map(|content| content.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("")
}

fn print_header(config: &BenchConfig, run_id: &str) {
    println!("Responses transport benchmark");
    println!("model: {MODEL}");
    println!("run: {run_id}");
    println!(
        "turns: {}, forks: {:?}, mainline continuations: {}, prefix facts: {}, repeats: {}",
        config.turns,
        config.fork_turns,
        config.mainline_continuations,
        config.prefix_facts,
        config.repeats
    );
}

fn print_variant_summary(measurement: &VariantMeasurement) {
    let fork_bytes: usize = measurement
        .branches
        .iter()
        .map(|branch| branch.response.request_bytes)
        .sum();
    let fork_median = {
        let mut values = measurement
            .branches
            .iter()
            .map(|branch| branch.response.response_ms)
            .collect::<Vec<_>>();
        values.sort_by(f64::total_cmp);
        median(&values)
    };
    println!(
        "  chain: {:>8.1} ms wall, {:>7.1} ms median response, {:>8} request B, {:>5.1}% cached",
        measurement.chain_wall_ms,
        measurement.chain_median_response_ms,
        measurement.chain_request_bytes,
        percentage(
            measurement.chain_usage.cached,
            measurement.chain_usage.input
        )
    );
    println!(
        "  race:  {:>8.1} ms mainline + forks, {:>7.1} ms median fork response, {:>8} fork request B, snapshot clone {:.1} us",
        measurement.mainline_and_forks_wall_ms,
        fork_median,
        fork_bytes,
        measurement.fork_snapshot_clone_us
    );
}

fn selected_variants() -> Result<Vec<Variant>> {
    let Ok(value) = std::env::var("FORK_BENCH_VARIANTS") else {
        return Ok(VARIANTS.to_vec());
    };
    if value.trim() == "all" {
        return Ok(VARIANTS.to_vec());
    }
    let mut selected = Vec::new();
    for name in value
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        let variant = VARIANTS
            .iter()
            .find(|variant| variant.name == name)
            .copied()
            .ok_or_else(|| {
                eyre!(
                    "unknown variant {name:?}; expected one of {}",
                    VARIANTS
                        .iter()
                        .map(|variant| variant.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
        if !selected
            .iter()
            .any(|existing: &Variant| existing.name == name)
        {
            selected.push(variant);
        }
    }
    if selected.is_empty() {
        bail!("FORK_BENCH_VARIANTS selected no variants");
    }
    Ok(selected)
}

fn env_usize(name: &str, default: usize) -> Result<usize> {
    match std::env::var(name) {
        Ok(value) => parse_positive_usize(name, &value),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(error).wrap_err_with(|| format!("failed to read {name}")),
    }
}

fn env_usize_list(name: &str, default: &[usize]) -> Result<Vec<usize>> {
    let Ok(value) = std::env::var(name) else {
        return Ok(default.to_vec());
    };
    let mut values = value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| parse_positive_usize(name, value))
        .collect::<Result<Vec<_>>>()?;
    values.sort_unstable();
    values.dedup();
    if values.is_empty() {
        bail!("{name} must contain at least one turn");
    }
    Ok(values)
}

fn parse_positive_usize(name: &str, value: &str) -> Result<usize> {
    let parsed = value
        .parse::<usize>()
        .wrap_err_with(|| format!("{name} must contain positive integers"))?;
    if parsed == 0 {
        bail!("{name} values must be greater than zero");
    }
    Ok(parsed)
}

fn run_id() -> Result<String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .wrap_err("system clock is before the Unix epoch")?
        .as_nanos();
    Ok(format!("{nanos:x}-{:x}", process::id()))
}

async fn cleanup_responses(config: &BenchConfig, response_ids: &[String]) {
    if response_ids.is_empty() {
        return;
    }
    let endpoint = config
        .http_endpoint
        .strip_suffix("/responses")
        .unwrap_or(&config.http_endpoint);
    let client = reqwest::Client::new();
    let mut deleted = 0;
    for response_id in response_ids.iter().rev() {
        let result = client
            .delete(format!("{endpoint}/responses/{response_id}"))
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
        "cleanup: deleted {deleted}/{} stored responses",
        response_ids.len()
    );
}

fn median(values: &[f64]) -> f64 {
    match values.len() {
        0 => 0.0,
        len if len % 2 == 1 => values[len / 2],
        len => f64::midpoint(values[len / 2 - 1], values[len / 2]),
    }
}

#[allow(clippy::cast_precision_loss)]
fn percentage(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 * 100.0 / whole as f64
    }
}

#[allow(clippy::cast_precision_loss)]
fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

#[allow(clippy::cast_precision_loss)]
fn duration_us(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_matrix_covers_every_supported_policy_combination() {
        assert_eq!(VARIANTS.len(), 7);
        assert!(VARIANTS.iter().any(|variant| {
            variant.name == "ws-ephemeral-connection"
                && !variant.store
                && matches!(variant.chain_history, HistoryPolicy::PreviousResponseId)
                && matches!(variant.fork_history, HistoryPolicy::FullReplay)
        }));
        assert!(!VARIANTS.iter().any(|variant| {
            matches!(variant.transport, Transport::Https)
                && !variant.store
                && matches!(variant.chain_history, HistoryPolicy::PreviousResponseId)
        }));
    }

    #[test]
    fn fork_history_snapshots_share_the_committed_prefix() {
        let history = History::new(user_message("one"));
        let snapshot = history.clone();
        assert!(Arc::ptr_eq(&history.head, &snapshot.head));

        let output: Arc<[Arc<Value>]> = Arc::from([Arc::new(user_message("output"))]);
        let branch = snapshot.append(Arc::new(user_message("two")), &output);
        assert!(Arc::ptr_eq(
            branch.head.previous.as_ref().unwrap(),
            &history.head
        ));
        assert_eq!(history.refs().len(), 1);
        assert_eq!(branch.refs().len(), 3);
    }

    #[test]
    fn websocket_and_https_requests_use_their_native_envelopes() {
        let input_value = user_message("hello");
        let input = [input_value];
        let input = input.iter().collect::<Vec<_>>();
        let request = |transport: Transport| RequestBody {
            kind: matches!(transport, Transport::WebSocket).then_some("response.create"),
            model: MODEL,
            previous_response_id: None,
            input: &input,
            tool_choice: "auto",
            parallel_tool_calls: false,
            reasoning: Reasoning {
                effort: "low",
                context: "all_turns",
            },
            store: false,
            stream: true,
            include: ["reasoning.encrypted_content"],
            prompt_cache_key: "cache",
            text: TextControls { verbosity: "low" },
            client_metadata: ClientMetadata {
                session_id: "session",
                thread_id: "session",
                ws_request_header_x_openai_internal_codex_responses_lite: matches!(
                    transport,
                    Transport::WebSocket
                )
                .then_some("true"),
            },
        };
        let websocket = serde_json::to_value(request(Transport::WebSocket)).unwrap();
        let https = serde_json::to_value(request(Transport::Https)).unwrap();

        assert_eq!(websocket["type"], "response.create");
        assert_eq!(
            websocket["client_metadata"]["ws_request_header_x_openai_internal_codex_responses_lite"],
            "true"
        );
        assert!(https.get("type").is_none());
        assert!(
            https["client_metadata"]
                .get("ws_request_header_x_openai_internal_codex_responses_lite")
                .is_none()
        );
        assert_eq!(https["store"], false);
    }
}
