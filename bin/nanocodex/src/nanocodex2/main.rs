//! Small managed-agent CLI with a local workspace tool host.
mod host;

use std::{
    env,
    io::{self, IsTerminal, Write},
    process::ExitCode,
};

use clap::{Args, Parser, Subcommand, builder::NonEmptyStringValueParser};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{self, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use host::HostConfig;
use nanocodex_agent::{AgentEvents, Nanocodex, NanocodexError, PromptRequest, Turn, TurnResult};
use nanocodex_managed::{
    AgentList, EventCursor, Managed, ManagedApiKey, ManagedClient, ManagedError, PromptInput,
    ReadSessionRequest, ReadSessionResponse,
};
use nanocodex_tools::{Tools, WorkspaceTools};
use tokio::io::{AsyncBufReadExt, BufReader};
use unicode_width::UnicodeWidthChar;

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
    /// Attach local workspace tools to an existing managed agent.
    Attach(Attach),
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
}

#[derive(Args)]
struct Attach {
    /// Account-owned managed agent ID. Choose from a list when omitted.
    #[arg(value_parser = NonEmptyStringValueParser::new())]
    agent_id: Option<String>,
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
    let client = client_from_environment()?;
    match cli.command {
        Some(Command::Attach(command)) => attach_tui(&client, command.agent_id).await,
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
        None => new_tui(&client).await,
    }
}

fn client_from_environment() -> Result<ManagedClient, ManagedError> {
    let base_url = managed_url_from_environment()?;
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

fn managed_url_from_environment() -> Result<String, ManagedError> {
    match env::var(MANAGED_URL_ENV) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        Ok(_) => Err(ManagedError::Configuration(format!(
            "{MANAGED_URL_ENV} must not be empty"
        ))),
        Err(env::VarError::NotPresent) => Ok(DEFAULT_MANAGED_ORIGIN.to_owned()),
        Err(env::VarError::NotUnicode(_)) => Err(ManagedError::Configuration(format!(
            "{MANAGED_URL_ENV} must be valid Unicode"
        ))),
    }
}

async fn run_turn(client: &ManagedClient, command: Run) -> Result<(), ManagedError> {
    let agent_id = match command.agent {
        Some(agent_id) => agent_id,
        None => {
            let agent_id = client.create().await?.agent_id;
            eprintln!("Managed agent: {agent_id}");
            agent_id
        }
    };
    let (agent, mut events, _, _) = open_workspace_agent(client, Some(agent_id)).await?;
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
    let agent_id = match requested_agent_id {
        Some(agent_id) => agent_id,
        None => match choose_agent(&client.list().await?)? {
            Some(agent_id) => agent_id,
            None => return Ok(()),
        },
    };
    let state = client.state(&agent_id).await?;
    let history = client
        .read(&ReadSessionRequest {
            session_id: state.session_id,
            turn_ids: None,
        })
        .await?;
    run_tui(client, agent_id, Some(history)).await
}

async fn new_tui(client: &ManagedClient) -> Result<(), ManagedError> {
    let agent_id = client.create().await?.agent_id;
    run_tui(client, agent_id, None).await
}

async fn run_tui(
    client: &ManagedClient,
    agent_id: String,
    history: Option<ReadSessionResponse>,
) -> Result<(), ManagedError> {
    let (agent, events, agent_id, workspace) = open_workspace_agent(client, Some(agent_id)).await?;
    drop(events);

    println!("Nanocodex2");
    println!("agent     {agent_id}");
    println!("workspace {}", workspace.display());
    println!("Type /exit or press Ctrl-D to quit.\n");
    if let Some(history) = history {
        print_recent_history(&history);
    }

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        print!("› ");
        io::stdout()
            .flush()
            .map_err(|_| ManagedError::InvalidResponse("failed to write prompt"))?;
        let Some(line) = lines
            .next_line()
            .await
            .map_err(|_| ManagedError::InvalidResponse("failed to read prompt"))?
        else {
            println!();
            break;
        };
        let prompt = line.trim();
        if prompt.is_empty() {
            continue;
        }
        if matches!(prompt, "/exit" | "/quit") {
            break;
        }

        let turn = agent
            .prompt(PromptRequest::new(prompt.to_owned()))
            .await
            .map_err(agent_error)?;
        tokio::pin!(turn);
        let result = tokio::select! {
            result = &mut turn => Some(result.map_err(agent_error)?),
            signal = tokio::signal::ctrl_c() => {
                signal.map_err(|error| ManagedError::Configuration(
                    format!("failed to listen for Ctrl-C: {error}")
                ))?;
                None
            }
        };
        let Some(result) = result else {
            println!("\nDetached. The durable turn is still running.");
            return agent.disconnect().await.map_err(agent_error);
        };
        println!("\n{}\n", result.final_message());
    }

    agent.shutdown().await.map_err(agent_error)
}

fn choose_agent(agents: &AgentList) -> Result<Option<String>, ManagedError> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(ManagedError::Configuration(
            "agent selection requires a terminal; use `nanocodex2 attach <agent-id>`".to_owned(),
        ));
    }
    if agents.data.is_empty() {
        return Err(ManagedError::Configuration(
            "there are no existing agents to attach to; run `nanocodex2` to create one".to_owned(),
        ));
    }

    let _terminal = SelectorTerminal::enter().map_err(terminal_error)?;
    let mut selected = 0;
    let option_count = agents.data.len();
    loop {
        render_agent_selector(agents, selected).map_err(terminal_error)?;
        let Event::Key(key) = event::read().map_err(terminal_error)? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Up => selected = selected.checked_sub(1).unwrap_or(option_count - 1),
            KeyCode::Down => selected = (selected + 1) % option_count,
            KeyCode::Home => selected = 0,
            KeyCode::End => selected = option_count - 1,
            KeyCode::Enter => return Ok(Some(agents.data[selected].clone())),
            KeyCode::Esc | KeyCode::Char('q') => return Ok(None),
            _ => {}
        }
    }
}

fn render_agent_selector(agents: &AgentList, selected: usize) -> io::Result<()> {
    let mut output = io::stdout();
    let (width, height) = terminal::size()?;
    let width = usize::from(width);
    let visible = usize::from(height.saturating_sub(6)).max(1);
    let start = selected
        .saturating_sub(visible / 2)
        .min(agents.data.len().saturating_sub(visible));
    let end = (start + visible).min(agents.data.len());
    execute!(
        output,
        cursor::MoveTo(0, 0),
        terminal::Clear(ClearType::All)
    )?;
    terminal_line(
        &mut output,
        &format!("Select an agent  ({}/{})", selected + 1, agents.data.len()),
        width,
    )?;
    terminal_line(&mut output, "", width)?;
    for index in start..end {
        let agent_id = &agents.data[index];
        let (title, turn_count) = agents
            .summaries
            .get(agent_id)
            .map(|summary| {
                let title = summary.title.lines().next().unwrap_or("Untitled").trim();
                (
                    if title.is_empty() { "Untitled" } else { title },
                    summary.turn_count,
                )
            })
            .unwrap_or(("Untitled", 0));
        let turns = if turn_count == 1 { "turn" } else { "turns" };
        let row = if index == selected {
            format!("› ◉ {title}  · {turn_count} {turns}")
        } else {
            format!("  ○ {title}  · {turn_count} {turns}")
        };
        terminal_line(&mut output, &row, width)?;
    }
    terminal_line(&mut output, "", width)?;
    terminal_line(
        &mut output,
        &format!("Agent: {}", agents.data[selected]),
        width,
    )?;
    terminal_line(&mut output, "↑/↓ choose  Enter attach  Esc cancel", width)?;
    output.flush()
}

fn terminal_line(output: &mut impl Write, line: &str, width: usize) -> io::Result<()> {
    let line = truncate_to_width(line, width);
    write!(output, "{line}\r\n")
}

fn truncate_to_width(value: &str, max_width: usize) -> String {
    let width = value
        .chars()
        .map(|character| character.width().unwrap_or(0))
        .sum::<usize>();
    if width <= max_width {
        return value.to_owned();
    }
    if max_width == 0 {
        return String::new();
    }

    let mut truncated = String::new();
    let mut width = 0;
    for character in value.chars() {
        let character_width = character.width().unwrap_or(0);
        if width + character_width >= max_width {
            break;
        }
        truncated.push(character);
        width += character_width;
    }
    truncated.push('…');
    truncated
}

fn print_recent_history(history: &ReadSessionResponse) {
    if history.turns.is_empty() {
        println!("No conversation history.\n");
        return;
    }

    println!("Recent history");
    if history.turns.len() == 20 {
        println!("… showing the latest 20 turns\n");
    } else {
        println!();
    }
    for turn in &history.turns {
        println!("You\n{}\n", turn.user);
        println!("Nanocodex\n{}\n", turn.assistant);
    }
}

struct SelectorTerminal;

impl SelectorTerminal {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        if let Err(error) = execute!(io::stdout(), EnterAlternateScreen, cursor::Hide) {
            let _ = terminal::disable_raw_mode();
            return Err(error);
        }
        Ok(Self)
    }
}

impl Drop for SelectorTerminal {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), cursor::Show, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

fn terminal_error(error: io::Error) -> ManagedError {
    ManagedError::Configuration(format!("terminal error: {error}"))
}

async fn open_workspace_agent(
    client: &ManagedClient,
    agent_id: Option<String>,
) -> Result<(Nanocodex, AgentEvents, String, std::path::PathBuf), ManagedError> {
    let config =
        HostConfig::load().map_err(|error| ManagedError::Configuration(error.to_string()))?;
    let workspace = config.workspace().to_path_buf();
    let tools = Tools::builder()
        .without_defaults()
        .add(WorkspaceTools::new(&workspace))
        .build()
        .map_err(|error| ManagedError::Configuration(error.to_string()))?;
    let agent_id = match agent_id {
        Some(agent_id) => agent_id,
        None => client.create().await?.agent_id,
    };
    let backend = Managed::open(client.clone(), agent_id.clone());
    let (agent, events) = Nanocodex::builder(backend)
        .tools(tools)
        .build()
        .await
        .map_err(agent_error)?;
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
            result = &mut turn => {
                let result = result.map_err(agent_error)?;
                while let Some(event) = events.try_recv_timed() {
                    write_json_line(&event.event)?;
                }
                return Ok(Some(result));
            }
            event = events.recv() => match event {
                Some(event) => {
                    write_json_line(&event)?;
                }
                None => return Err(ManagedError::Configuration(
                    "agent event stream stopped before turn completion".to_owned(),
                )),
            },
            signal = &mut interrupt => {
                signal.map_err(|error| ManagedError::Configuration(
                    format!("failed to listen for Ctrl-C: {error}")
                ))?;
                return Ok(None);
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
