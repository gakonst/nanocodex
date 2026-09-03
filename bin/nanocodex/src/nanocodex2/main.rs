//! Managed-agent CLI with a Tact-derived local terminal interface.
#![allow(
    clippy::missing_const_for_fn,
    clippy::too_many_arguments,
    clippy::use_self,
    reason = "preserve the reviewed Tact component ownership while adapting its engine boundary"
)]

#[allow(dead_code)]
mod config;
mod host;
#[allow(dead_code)]
mod installation;
#[allow(dead_code)]
mod skill;
#[allow(dead_code, unused_imports)]
mod tui;
#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
mod vm_hand;
#[cfg(not(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
)))]
#[path = "vm_hand_unsupported.rs"]
mod vm_hand;

use std::{
    env,
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};

use clap::{Args, Parser, Subcommand, builder::NonEmptyStringValueParser};
use host::HostConfig;
use nanocodex_agent::{AgentEvents, Nanocodex, NanocodexError, PromptRequest, Turn, TurnResult};
use nanocodex_managed::{
    AgentSettings, AgentState, EventCursor, Managed, ManagedApiKey, ManagedClient, ManagedError,
    ManagedEvent, PromptInput,
};
use nanocodex_tools::{
    Tools, WorkspaceTools,
    attachment::{Attachment, AttachmentMetadata, AttachmentTarget},
};
use percent_encoding::percent_decode_str;
use url::Url;

const MANAGED_URL_ENV: &str = "NANOCODEX_MANAGED_URL";
const API_KEY_ENV: &str = "NANOCODEX_API_KEY";
const API_KEY_FALLBACK_ENV: &str = "NC_API_KEY";
const DEFAULT_MANAGED_ORIGIN: &str = "https://nanocodex.gakonst.workers.dev";

#[derive(Parser)]
#[command(
    name = "nanocodex2",
    about = "Small managed Nanocodex client with local workspace tools"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Attach this machine's workspace to an existing managed agent.
    Attach(Attach),
    /// Register one retained libkrun VM as a compute hand for the account.
    Hand(Hand),
    /// Create a managed agent and print its receipt as JSON.
    New,
    /// List account-owned managed agents as JSON.
    List,
    /// Read one managed agent's durable state as JSON.
    State(AgentId),
    /// Read one managed turn's durable state as JSON.
    Turn(TurnId),
    /// Delete one managed agent and its retained state.
    Delete(AgentId),
    /// Submit one prompt and stream durable managed events as JSONL.
    Run(Run),
    /// Stream an owned agent's durable events from a cursor.
    Watch(Watch),
    /// Read one backward page of retained events.
    History(History),
    /// Steer an active managed turn.
    Steer(Steer),
    /// Cancel an active managed turn.
    Cancel(TurnId),
    /// Private synchronous entrypoint used by the VM hand's VMM child.
    #[command(name = "__vm-run-config", hide = true)]
    VmRunConfig(VmRunConfig),
}

#[derive(Args)]
struct Attach {
    /// Account-owned agent URL or ID. Choose from a list when omitted.
    #[arg(value_name = "AGENT_URL_OR_ID", value_parser = parse_agent_reference)]
    agent: Option<AgentReference>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AgentReference {
    agent_id: String,
    managed_origin: Option<String>,
}

#[derive(Args)]
struct Hand {
    /// Writable raw ext4 image or development directory used as the retained VM root.
    #[arg(long = "vm", visible_alias = "vm-rootfs", value_name = "ROOTFS")]
    rootfs: PathBuf,

    /// Statically linked Linux guest executable used with a raw ext4 root.
    #[arg(long, value_name = "ELF", env = "NANOCODEX_VM_GUEST_RUNTIME")]
    vm_guest_runtime: Option<PathBuf>,

    /// Cache for the prepared read-only guest runtime disk.
    #[arg(long, value_name = "PATH", default_value = ".cache/vm")]
    vm_cache: PathBuf,

    /// Directory containing the platform libkrun firmware library.
    #[arg(long, value_name = "PATH", env = "NANOCODEX_KRUNFW_DIR")]
    vm_firmware: Option<PathBuf>,

    /// Absolute working directory inside the VM.
    #[arg(long, value_name = "PATH", default_value = "/app")]
    vm_workspace: String,

    /// Number of virtual CPUs assigned to the hand.
    #[arg(long, value_name = "COUNT", default_value_t = 2, value_parser = clap::value_parser!(u8).range(1..))]
    vm_cpus: u8,

    /// Guest memory in mebibytes.
    #[arg(long, value_name = "MIB", default_value_t = 1_024, value_parser = clap::value_parser!(u32).range(1..))]
    vm_memory_mib: u32,

    /// Shell name described to the managed brain.
    #[arg(long, value_name = "SHELL", default_value = "sh")]
    vm_shell: String,

    /// Disable guest internet socket proxying.
    #[arg(long)]
    vm_no_network: bool,

    /// Stable account-local machine identifier.
    #[arg(long, default_value = "vm")]
    machine_id: String,

    /// Human-readable name shown in accountInfo().machines.
    #[arg(long, default_value = "Nanocodex VM")]
    machine_name: String,
}

#[derive(Args)]
struct VmRunConfig {
    /// Mode-0600 launch record prepared by nanocodex-vm.
    #[arg(long)]
    config: PathBuf,
}

#[derive(Args)]
struct AgentId {
    /// Account-owned managed agent ID.
    agent_id: String,
}

#[derive(Args)]
struct TurnId {
    /// Account-owned managed agent ID.
    agent_id: String,
    /// Managed turn ID.
    turn_id: String,
}

#[derive(Args)]
struct Run {
    /// Prompt text.
    #[arg(value_parser = NonEmptyStringValueParser::new())]
    prompt: String,
    /// Resume this account-owned agent. A new one is created when omitted.
    #[arg(long)]
    agent: Option<String>,
    /// Stable idempotency key. The managed backend generates one when omitted.
    #[arg(long)]
    idempotency_key: Option<String>,
}

#[derive(Args)]
struct Watch {
    /// Account-owned managed agent ID.
    agent_id: String,
    /// Resume strictly after this decimal cursor, or tail from `latest`.
    #[arg(long, default_value = "0")]
    cursor: String,
}

#[derive(Args)]
struct History {
    /// Account-owned managed agent ID.
    agent_id: String,
    /// Return rows strictly before this positive decimal cursor.
    #[arg(long)]
    before: Option<String>,
    /// Page size from 1 through 256.
    #[arg(long, default_value_t = 128)]
    limit: u16,
}

#[derive(Args)]
struct Steer {
    /// Account-owned managed agent ID.
    agent_id: String,
    /// Active managed turn ID.
    turn_id: String,
    /// Additional prompt text.
    #[arg(value_parser = NonEmptyStringValueParser::new())]
    prompt: String,
}

fn main() -> ExitCode {
    match try_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn try_main() -> Result<(), ManagedError> {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| ManagedError::Configuration(format!("failed to start Tokio: {error}")))?
        .block_on(run(cli))
}

async fn run(cli: Cli) -> Result<(), ManagedError> {
    if let Some(Command::VmRunConfig(command)) = &cli.command {
        return vm_hand::run_config(&command.config);
    }
    let managed_origin = match &cli.command {
        Some(Command::Attach(Attach { agent: Some(agent) })) => agent.managed_origin.as_deref(),
        _ => None,
    };
    let client = client_from_environment(managed_origin)?;
    match cli.command {
        Some(Command::Attach(command)) => {
            attach_tui(&client, command.agent.map(|agent| agent.agent_id)).await
        }
        Some(Command::Hand(command)) => serve_vm_hand(&client, command).await,
        Some(Command::New) => write_json(&client.create().await?),
        Some(Command::List) => write_json(&client.list().await?),
        Some(Command::State(command)) => write_json(&client.state(&command.agent_id).await?),
        Some(Command::Turn(command)) => write_json(
            &client
                .turn_state(&command.agent_id, &command.turn_id)
                .await?,
        ),
        Some(Command::Delete(command)) => client.delete(&command.agent_id).await,
        Some(Command::Run(command)) => run_turn(&client, command).await,
        Some(Command::Watch(command)) => watch(&client, command).await,
        Some(Command::History(command)) => write_json(
            &client
                .history(&command.agent_id, command.before.as_deref(), command.limit)
                .await?,
        ),
        Some(Command::Steer(command)) => write_json(
            &client
                .steer(
                    &command.agent_id,
                    &command.turn_id,
                    &PromptInput::Text(command.prompt),
                )
                .await?,
        ),
        Some(Command::Cancel(command)) => {
            write_json(&client.cancel(&command.agent_id, &command.turn_id).await?)
        }
        Some(Command::VmRunConfig(_)) => unreachable!("handled before managed client setup"),
        None => new_tui(&client).await,
    }
}

async fn serve_vm_hand(client: &ManagedClient, command: Hand) -> Result<(), ManagedError> {
    let target = client.account_attachment_target()?;
    let hand = vm_hand::VmHand::start(&command).await?;
    let connected = connect_vm_hand(&hand, target).await;
    let attachment = match connected {
        Ok(Some(attachment)) => attachment,
        Ok(None) => {
            hand.shutdown().await?;
            return Ok(());
        }
        Err(error) => {
            return match hand.shutdown().await {
                Ok(()) => Err(error),
                Err(shutdown) => Err(ManagedError::Configuration(format!(
                    "{error}; VM shutdown also failed: {shutdown}"
                ))),
            };
        }
    };

    eprintln!(
        "VM hand {} registered for the account at {} ({} vCPU, {} MiB); press Ctrl-C to detach",
        hand.machine().name(),
        hand.machine().workspace(),
        command.vm_cpus,
        command.vm_memory_mib,
    );
    let closed = attachment.clone();
    let attachment_result = tokio::select! {
        signal = tokio::signal::ctrl_c() => {
            signal.map_err(|error| ManagedError::Configuration(
                format!("failed to listen for Ctrl-C: {error}")
            ))?;
            attachment.clone().detach().await
        }
        result = closed.closed() => result,
    };
    drop(attachment);
    drop(closed);
    let shutdown = hand.shutdown().await;
    match (attachment_result, shutdown) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(ManagedError::Configuration(error.to_string())),
        (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(shutdown)) => Err(ManagedError::Configuration(format!(
            "{error}; VM shutdown also failed: {shutdown}"
        ))),
    }
}

async fn connect_vm_hand(
    hand: &vm_hand::VmHand,
    target: AttachmentTarget,
) -> Result<Option<Attachment>, ManagedError> {
    let connector = hand
        .tools()
        .attach(target)
        .metadata(AttachmentMetadata::machine(hand.machine().clone()));
    tokio::select! {
        signal = tokio::signal::ctrl_c() => {
            signal.map_err(|error| ManagedError::Configuration(
                format!("failed to listen for Ctrl-C: {error}")
            ))?;
            Ok(None)
        }
        connected = connector.connect() => connected
            .map(|(attachment, _events)| Some(attachment))
            .map_err(|error| ManagedError::Configuration(error.to_string())),
    }
}

fn client_from_environment(url_origin: Option<&str>) -> Result<ManagedClient, ManagedError> {
    let base_url = managed_url_from_environment(url_origin)?;
    let api_key = api_key_from_environment().map_err(|_| {
        ManagedError::Configuration(format!(
            "{API_KEY_ENV} (or {API_KEY_FALLBACK_ENV}) must be set to an account-issued ncx_live key"
        ))
    })?;
    ManagedClient::new(base_url, ManagedApiKey::parse(api_key)?)
}

fn api_key_from_environment() -> Result<String, env::VarError> {
    env::var(API_KEY_ENV).or_else(|_| env::var(API_KEY_FALLBACK_ENV))
}

fn managed_url_from_environment(fallback_origin: Option<&str>) -> Result<String, ManagedError> {
    match env::var(MANAGED_URL_ENV) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        Ok(_) => Err(ManagedError::Configuration(format!(
            "{MANAGED_URL_ENV} must not be empty"
        ))),
        Err(env::VarError::NotPresent) => {
            Ok(fallback_origin.unwrap_or(DEFAULT_MANAGED_ORIGIN).to_owned())
        }
        Err(env::VarError::NotUnicode(_)) => Err(ManagedError::Configuration(format!(
            "{MANAGED_URL_ENV} must be valid Unicode"
        ))),
    }
}

fn parse_agent_reference(value: &str) -> Result<AgentReference, String> {
    if valid_managed_agent_id(value) {
        return Ok(AgentReference {
            agent_id: value.to_owned(),
            managed_origin: None,
        });
    }
    let url = Url::parse(value).map_err(|_| {
        "agent must be a managed agent ID or a Nanocodex /agent/<agent-id> URL".to_owned()
    })?;
    if !supported_agent_page_origin(&url) || !url.username().is_empty() || url.password().is_some()
    {
        return Err("agent URL must use a Nanocodex web origin without credentials".to_owned());
    }
    let segments = url
        .path_segments()
        .ok_or_else(|| "agent URL must have the path /agent/<agent-id>".to_owned())?
        .collect::<Vec<_>>();
    let encoded = match segments.as_slice() {
        ["agent", encoded] if !encoded.is_empty() => *encoded,
        ["agent", encoded, ""] if !encoded.is_empty() => *encoded,
        _ => return Err("agent URL must have the path /agent/<agent-id>".to_owned()),
    };
    let agent_id = percent_decode_str(encoded)
        .decode_utf8()
        .map_err(|_| "agent URL contains an invalid UTF-8 path segment".to_owned())?;
    if !valid_managed_agent_id(&agent_id) {
        return Err("agent URL contains an invalid managed agent ID".to_owned());
    }
    Ok(AgentReference {
        agent_id: agent_id.into_owned(),
        managed_origin: Some(url.origin().ascii_serialization()),
    })
}

fn valid_managed_agent_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn supported_agent_page_origin(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if url.scheme() == "https" && host == "nanocodex.gakonst.workers.dev" && url.port().is_none() {
        return true;
    }
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    host == "nanocodex.localhost"
        || host
            .strip_suffix(".nanocodex.localhost")
            .is_some_and(|label| {
                !label.is_empty()
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                    })
            })
}

async fn run_turn(client: &ManagedClient, command: Run) -> Result<(), ManagedError> {
    let created = command.agent.is_none();
    let (agent, mut events, agent_id, _) =
        open_workspace_agent_from(client, command.agent, None, None).await?;
    if created {
        eprintln!("Managed agent: {agent_id}");
    }
    let mut request = PromptRequest::new(command.prompt);
    if let Some(request_id) = command.idempotency_key {
        request = request.request_id(request_id);
    }
    let turn: Turn = agent.prompt(request).await.map_err(agent_error)?;
    let outcome = await_turn(turn, &mut events).await;
    let shutdown = agent.shutdown().await.map_err(agent_error);
    match (outcome, shutdown) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(Some(result)), Ok(())) => {
            eprintln!("{}", result.final_message());
            Ok(())
        }
        (Ok(None), Ok(())) => Ok(()),
    }
}

async fn attach_tui(
    client: &ManagedClient,
    requested_agent_id: Option<String>,
) -> Result<(), ManagedError> {
    tui::run(client, requested_agent_id).await
}

async fn new_tui(client: &ManagedClient) -> Result<(), ManagedError> {
    tui::run_new(client).await
}

async fn open_workspace_agent_from(
    client: &ManagedClient,
    agent_id: Option<String>,
    state: Option<AgentState>,
    event_observer: Option<tokio::sync::mpsc::UnboundedSender<ManagedEvent>>,
) -> Result<(Nanocodex, AgentEvents, String, std::path::PathBuf), ManagedError> {
    open_workspace_agent_with_settings(
        client,
        agent_id,
        state,
        AgentSettings::default(),
        event_observer,
    )
    .await
}

async fn open_workspace_agent_with_settings(
    client: &ManagedClient,
    agent_id: Option<String>,
    state: Option<AgentState>,
    settings: AgentSettings,
    event_observer: Option<tokio::sync::mpsc::UnboundedSender<ManagedEvent>>,
) -> Result<(Nanocodex, AgentEvents, String, std::path::PathBuf), ManagedError> {
    let config =
        HostConfig::load().map_err(|error| ManagedError::Configuration(error.to_string()))?;
    let workspace = config.workspace().to_path_buf();
    let attachment_metadata = config
        .attachment_metadata()
        .map_err(|error| ManagedError::Configuration(error.to_string()))?;
    let tools = Tools::builder()
        .without_defaults()
        .add(WorkspaceTools::new(&workspace))
        .build()
        .map_err(|error| ManagedError::Configuration(error.to_string()))?;
    let backend = match (agent_id, state) {
        (None, None) => Managed::create_live(client.clone()).with_settings(settings),
        (Some(agent_id), Some(state)) => {
            Managed::open_live_from_state(client.clone(), agent_id, state)
        }
        (Some(agent_id), None) => Managed::open_live(client.clone(), agent_id),
        (None, Some(_)) => {
            return Err(ManagedError::Configuration(
                "managed state requires an agent identifier".to_owned(),
            ));
        }
    };
    let builder = Nanocodex::builder(backend)
        .tools(tools)
        .attachment_metadata(attachment_metadata);
    let builder = match event_observer {
        Some(observer) => builder.event_observer(observer),
        None => builder,
    };
    let (agent, events) = builder.build().await.map_err(agent_error)?;
    let agent_id = agent.agent_id().to_owned();
    Ok((agent, events, agent_id, workspace))
}

async fn await_turn(
    turn: Turn,
    events: &mut AgentEvents,
) -> Result<Option<TurnResult>, ManagedError> {
    tokio::pin!(turn);
    let interrupt = tokio::signal::ctrl_c();
    tokio::pin!(interrupt);
    loop {
        tokio::select! {
            biased;
            result = &mut turn => {
                let result = result.map_err(agent_error)?;
                while let Some(event) = events.try_recv_timed() {
                    write_json_line(&event.event)?;
                }
                return Ok(Some(result));
            }
            signal = &mut interrupt => {
                signal.map_err(|error| ManagedError::Configuration(
                    format!("failed to listen for Ctrl-C: {error}")
                ))?;
                return Ok(None);
            },
            event = events.recv() => match event {
                Some(event) => {
                    write_json_line(&event)?;
                }
                None => return tokio::select! {
                    result = &mut turn => result.map(Some).map_err(agent_error),
                    signal = &mut interrupt => {
                        signal.map_err(|error| ManagedError::Configuration(
                            format!("failed to listen for Ctrl-C: {error}")
                        ))?;
                        Ok(None)
                    },
                },
            },
        }
    }
}

fn agent_error(error: NanocodexError) -> ManagedError {
    ManagedError::Configuration(error.to_string())
}

async fn watch(client: &ManagedClient, command: Watch) -> Result<(), ManagedError> {
    let mut events = client.events(&command.agent_id, EventCursor::parse(command.cursor)?)?;
    loop {
        write_json_line(&events.next().await?)?;
    }
}

fn write_json<T: serde::Serialize>(value: &T) -> Result<(), ManagedError> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    serde_json::to_writer(&mut output, value)
        .map_err(|_| ManagedError::InvalidResponse("failed to encode output"))?;
    output
        .write_all(b"\n")
        .and_then(|()| output.flush())
        .map_err(|_| ManagedError::InvalidResponse("failed to write output"))
}

fn write_json_line<T: serde::Serialize>(value: &T) -> Result<(), ManagedError> {
    write_json(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_attach_url_into_its_agent_id() {
        let cli = Cli::try_parse_from([
            "nanocodex2",
            "attach",
            "https://named-workspace-fabric.nanocodex.localhost:2443/agent/77777777-7777-4777-8777-777777777777?thread=ignored#top",
        ])
        .expect("attach URL must parse");
        let Some(Command::Attach(Attach { agent })) = cli.command else {
            panic!("attach command parsed into the wrong variant");
        };
        assert_eq!(
            agent,
            Some(AgentReference {
                agent_id: "77777777-7777-4777-8777-777777777777".to_owned(),
                managed_origin: Some(
                    "https://named-workspace-fabric.nanocodex.localhost:2443".to_owned()
                ),
            })
        );
    }

    #[test]
    fn parses_raw_agent_ids_and_optional_picker() {
        assert_eq!(
            parse_agent_reference("agent:v1_test-id").unwrap(),
            AgentReference {
                agent_id: "agent:v1_test-id".to_owned(),
                managed_origin: None,
            }
        );
        let picker = Cli::try_parse_from(["nanocodex2", "attach"])
            .expect("attach without an agent must open the picker");
        assert!(matches!(
            picker.command,
            Some(Command::Attach(Attach { agent: None }))
        ));
    }

    #[test]
    fn parses_supported_agent_urls() {
        for (url, expected) in [
            (
                "https://nanocodex.gakonst.workers.dev/agent/agent-1",
                "agent-1",
            ),
            ("https://nanocodex.localhost/agent/a%3Ab/", "a:b"),
            ("http://nanocodex.localhost:5173/agent/local", "local"),
            ("https://branch-1.nanocodex.localhost/agent/id", "id"),
        ] {
            assert_eq!(
                parse_agent_reference(url).unwrap().agent_id,
                expected,
                "{url}"
            );
        }
    }

    #[test]
    fn rejects_non_agent_and_unsafe_urls() {
        for value in [
            "https://example.com/agent/id",
            "ftp://nanocodex.localhost/agent/id",
            "https://user@nanocodex.localhost/agent/id",
            "https://nanocodex.localhost/agent",
            "https://nanocodex.localhost/v1/agents/id",
            "https://nanocodex.localhost/agent/id/turns",
            "https://nanocodex.localhost/agent/a%2Fb",
            "https://nanocodex.localhost/agent/a%252Fb",
        ] {
            assert!(parse_agent_reference(value).is_err(), "{value}");
        }
    }
}
