//! The Nanocodex CLI and Hand, built as two role-split executables: `nanocodex`
//! ([`cli_main`]) and `nanocodex-hand` ([`hand_main`]).
#![recursion_limit = "256"]

mod auth;
mod benchmark;
mod browser;
mod browser_cookie_sync;
mod clipboard;
mod computer;
mod config;
#[cfg(feature = "tempo")]
mod credits;
#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
mod eval;
#[cfg(not(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
)))]
#[path = "eval_unsupported.rs"]
mod eval;
mod hand_executable;
#[cfg(target_os = "macos")]
mod hand_keep_awake;
mod hand_login;
mod hand_menu_bar;
mod hand_menu_status;
mod hand_registry;
mod hand_service;
mod hand_setup;
mod homes;
mod install;
mod launcher;
#[cfg(target_os = "linux")]
mod linux_hand_service;
mod login;
mod managed_memory;
mod managed_server;
mod mcp;
#[cfg_attr(not(feature = "tempo"), path = "mpp_disabled.rs")]
mod mpp;
mod nanocodex2;
/// Criterion groups over the shared TUI renderer, for `benches/nanocodex2_tui.rs`.
#[cfg(feature = "tui-bench")]
#[doc(hidden)]
pub use nanocodex2::tui::bench::tui_benches;
mod observability;
mod rewind;
mod rollout_fork;
mod run;
mod sessions;
mod setup;
mod startup_timing;
mod subagents;
mod tool_calls;
mod update;
mod version;
#[cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]
mod vm;
#[cfg(not(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
)))]
#[path = "vm_unsupported.rs"]
mod vm;
mod windows_hand;

use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{Args, CommandFactory, Parser, Subcommand, builder::NonEmptyStringValueParser};
use eyre::{Result, WrapErr, eyre};
use nanocodex::agent::rollout::RolloutConfig;

use config::AgentArgs;
use observability::ObservabilityArgs;

const RETRYABLE_EXIT_CODE: u8 = 75;

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
struct RetryableProcessExit {
    message: String,
}

impl RetryableProcessExit {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Parser)]
#[command(
    name = "ncl",
    version = version::SHORT_VERSION,
    long_version = version::LONG_VERSION,
    about = "An interactive coding agent and headless JSONL runner",
    subcommand_negates_reqs = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    agent: AgentArgs,

    #[command(flatten)]
    observability: ObservabilityArgs,

    #[command(flatten)]
    vm: vm::VmArgs,

    /// Submit an initial prompt immediately after the TUI opens.
    #[arg(long, value_parser = NonEmptyStringValueParser::new())]
    prompt: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Install the verified release bundle and start guided setup.
    Install(install::Install),
    /// Sign in and set up Computer Use, Hand, and the browser bridge.
    Setup(setup::Setup),
    /// Discover and control a running interactive terminal.
    Tui(nanocodex_tui_control::Cli),
    /// Install or refresh the upstream computer-use runtime.
    Computer(computer::Computer),
    /// Manage this computer’s Hand service or add a Linux Hand over SSH.
    Hand(hand_setup::Hand),
    /// Sign in to the managed Nanocodex account (same as `nanocodex login`).
    Account(nanocodex_cli_auth::Account),
    /// Manage subscription login for the selected harness.
    Auth(auth::Auth),
    /// Sign in to Nanocodex Connect and authorize this installation.
    Login(login::Login),
    /// Connect one or more hosted services to this Nanocodex installation.
    Connect(login::Connect),
    /// Show the current Nanocodex Connect login without displaying secrets.
    Status(login::Status),
    /// Revoke and remove this installation's Nanocodex Connect login.
    Logout(login::Logout),
    /// Inspect or synchronize local browser cookies and the encrypted account Vault.
    Cookies(browser_cookie_sync::Cookies),
    /// Inspect or purchase Nanocodex NANOUSD credits.
    #[cfg(feature = "tempo")]
    Credits(credits::Credits),
    /// Run and inspect durable VM-backed agent evaluations.
    Eval(eval::Eval),
    /// Internal entrypoint for one dedicated libkrun VMM process.
    #[command(hide = true)]
    VmRunConfig(vm::VmRunConfig),
    /// Run one prompt and stream JSONL events to stdout.
    Run(Box<RunCommand>),
    /// Run a loopback-only managed-agent durability test server.
    ManagedServer(managed_server::ManagedServer),
    /// Resume a saved session of any harness in the interactive TUI.
    Resume(Box<ResumeCommand>),
    /// Branch a saved session at an earlier turn, or restore its file checkpoints.
    Rewind(rewind::Rewind),
    /// Show the Codex and Claude homes and preview or create their shared links.
    Homes(homes::Homes),
    /// Install, cache, or switch CLI builds.
    Update(update::Update),
}

#[derive(Args)]
struct RunCommand {
    #[command(flatten)]
    run: run::Run,

    #[command(flatten)]
    agent: AgentArgs,

    #[command(flatten)]
    observability: ObservabilityArgs,

    #[command(flatten)]
    vm: vm::VmArgs,
}

#[derive(Args)]
struct ResumeCommand {
    /// Session ID to resume. Omit it to choose from the saved sessions of every
    /// harness; the session continues in the harness that recorded it.
    #[arg(value_parser = NonEmptyStringValueParser::new())]
    session: Option<String>,

    /// Start a new Codex thread from this rollout file instead of a saved session.
    ///
    /// The file is copied, never changed. The new thread's workspace is
    /// `--cwd`, or the current directory, so rollouts recorded elsewhere work.
    #[arg(long, value_name = "ROLLOUT", conflicts_with = "session")]
    from: Option<PathBuf>,

    /// Start from this point: a turn ID, or a completed-turn number from 1.
    ///
    /// Branches the session (in the harness that recorded it) or the `--from`
    /// rollout (as a new Codex thread) into a new session whose history ends
    /// after that turn. The original session and file are not changed.
    #[arg(long, value_name = "TURN", value_parser = NonEmptyStringValueParser::new())]
    at: Option<String>,

    #[command(flatten)]
    agent: AgentArgs,

    #[command(flatten)]
    observability: ObservabilityArgs,

    #[command(flatten)]
    vm: vm::VmArgs,

    /// Submit an initial follow-on prompt immediately after the TUI opens.
    #[arg(long, value_parser = NonEmptyStringValueParser::new())]
    prompt: Option<String>,
}

/// The command tree a process runs.
///
/// One executable serves every installed name. `nanocodex`, `nc`, and
/// `nanocodex2` select the managed tree; `ncl`, or a leading `--local`,
/// selects the local agent tree. Commands that only one tree defines, including
/// every hidden service entrypoint, run under every name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Tree {
    Managed,
    Local,
}

/// Entry point of the `nanocodex` CLI. Hand serving and daemon-side
/// entrypoints are forwarded to the installed Hand executable; the CLI never
/// runs them in-process.
pub fn cli_main() -> ExitCode {
    hand_executable::take_forwarded();
    let mut arguments: Vec<OsString> = std::env::args_os().collect();
    let tree = select_tree(&mut arguments);
    if is_daemon_command(&arguments) {
        if hand_executable::forwarded() {
            // The Hand forwarded this here, so it does not serve it either;
            // never bounce it back.
            eprintln!(
                "Error: this command is served by the Nanocodex Hand executable, which did not accept it"
            );
            return ExitCode::FAILURE;
        }
        return match hand_executable::hand_binary() {
            // Keep the invoked name so help and errors read as this command.
            Ok(hand) => {
                hand_executable::forward(&hand, arguments.first().cloned(), &arguments[1..])
            }
            Err(error) => {
                eprintln!("Error: {error}");
                ExitCode::FAILURE
            }
        };
    }
    match tree {
        Tree::Managed => nanocodex2::main(arguments),
        Tree::Local => local_main(arguments),
    }
}

/// Entry point of the `nanocodex-hand` daemon executable.
///
/// Only daemon commands (and the Hand's own `--version`/`--help`) run here;
/// this entry never reaches the CLI command trees or the terminal UI, so they
/// are not linked into the Hand. Older installations may point `bin/nanocodex2`
/// at this file, so every other invocation, including a leading `--local`, is
/// forwarded unchanged to the CLI installed beside it.
pub fn hand_main() -> ExitCode {
    hand_executable::set_hand_role();
    hand_executable::take_forwarded();
    let arguments: Vec<OsString> = std::env::args_os().collect();
    if nanocodex2::is_helper_process() || is_hand_daemon_invocation(&arguments) {
        return nanocodex2::daemon::main(arguments);
    }
    if hand_executable::forwarded() {
        // The CLI forwarded this here; never bounce it back.
        eprintln!("Error: this command is provided by the nanocodex CLI, not the Hand executable");
        return ExitCode::FAILURE;
    }
    match hand_executable::cli_binary() {
        Ok(cli) => hand_executable::forward(&cli, arguments.first().cloned(), &arguments[1..]),
        Err(error) => {
            eprintln!("Error: {error}; user commands are provided by the nanocodex CLI");
            ExitCode::FAILURE
        }
    }
}

/// Invocations the Hand executable serves itself, decided without the CLI
/// command trees: daemon entrypoints, `hand` serving (no management
/// subcommand, no help flag), and the Hand's bare `--version`/`--help`.
fn is_hand_daemon_invocation(arguments: &[OsString]) -> bool {
    let argument = |index: usize| arguments.get(index).and_then(|argument| argument.to_str());
    match argument(1) {
        Some("--version" | "-V" | "--help" | "-h") => arguments.len() == 2,
        Some(
            "__device-hand" | "__hand-screen" | "__hand-desktop" | "__install-hand"
            | "__update-hand" | "wayland-host" | "desktop-host" | "server-host" | "__vm-run-config"
            | "vm-run-config" | "__vm-clone-image" | "host",
        ) => true,
        // Serving flags start with `-`; a word is a CLI management subcommand.
        Some("hand") => {
            argument(2).is_none_or(|next| next.starts_with('-'))
                && !arguments[2..]
                    .iter()
                    .any(|argument| argument == "-h" || argument == "--help")
        }
        _ => false,
    }
}

/// Commands that serve a Hand or are internal entrypoints of the Hand daemon
/// (service protocol, screen/input publishers, VMM children, root helpers).
fn is_daemon_command(arguments: &[OsString]) -> bool {
    if nanocodex2::is_helper_process() {
        return true;
    }
    let argument = |index: usize| arguments.get(index).and_then(|argument| argument.to_str());
    match argument(1) {
        Some(
            "__device-hand" | "__hand-screen" | "__hand-desktop" | "__install-hand"
            | "__update-hand" | "wayland-host" | "desktop-host" | "server-host" | "__vm-run-config"
            | "vm-run-config" | "__vm-clone-image" | "host",
        ) => true,
        // `hand` alone (or with backend flags) serves; management subcommands
        // and help stay in the CLI.
        Some("hand") => {
            !argument(2).is_some_and(|name| name == "help" || is_hand_management_command(name))
                && !arguments[2..]
                    .iter()
                    .any(|argument| argument == "-h" || argument == "--help")
        }
        _ => false,
    }
}

fn is_hand_management_command(name: &str) -> bool {
    Cli::command()
        .find_subcommand("hand")
        .is_some_and(|hand| hand.find_subcommand(name).is_some())
}

/// Mode selected by the invoked name. Use argv[0], not `current_exe`, which
/// resolves the installed alias symlinks to one file.
fn invoked_tree(argv0: &OsStr) -> Tree {
    let name = Path::new(argv0)
        .file_name()
        .map(OsStr::to_string_lossy)
        .unwrap_or_default();
    let stem = name
        .len()
        .checked_sub(4)
        .filter(|split| {
            name.is_char_boundary(*split) && name[*split..].eq_ignore_ascii_case(".exe")
        })
        .map_or(&*name, |split| &name[..split]);
    if stem.eq_ignore_ascii_case("ncl") {
        Tree::Local
    } else {
        Tree::Managed
    }
}

/// Choose the tree before clap parses, stripping a leading `--local`.
fn select_tree(arguments: &mut Vec<OsString>) -> Tree {
    if nanocodex2::is_helper_process() {
        return Tree::Managed;
    }
    let mut tree = arguments
        .first()
        .map_or(Tree::Managed, |argv0| invoked_tree(argv0));
    if arguments
        .get(1)
        .is_some_and(|argument| argument == "--local")
    {
        arguments.remove(1);
        tree = Tree::Local;
    }
    let Some(first) = arguments.get(1).and_then(|argument| argument.to_str()) else {
        return tree;
    };
    let local = Cli::command();
    if first == "hand" {
        // `hand` alone serves this computer (managed flags); its management
        // subcommands keep the local service and update implementation.
        let management = arguments
            .get(2)
            .and_then(|argument| argument.to_str())
            .is_some_and(|name| {
                local
                    .find_subcommand("hand")
                    .is_some_and(|hand| hand.find_subcommand(name).is_some())
            });
        return if management {
            Tree::Local
        } else {
            Tree::Managed
        };
    }
    let managed = nanocodex2::command();
    match (
        tree,
        managed.find_subcommand(first).is_some(),
        local.find_subcommand(first).is_some(),
    ) {
        (Tree::Managed, false, true) => Tree::Local,
        (Tree::Local, true, false) => Tree::Managed,
        (tree, _, _) => tree,
    }
}

/// Add the other tree's visible, unambiguous commands to this tree's help so
/// every command reachable under this name is listed. Dispatch is by
/// [`select_tree`]; these copies only document it.
fn with_foreign_commands(mut command: clap::Command, foreign: &clap::Command) -> clap::Command {
    let additions: Vec<clap::Command> = foreign
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
        .filter(|subcommand| {
            std::iter::once(subcommand.get_name())
                .chain(subcommand.get_all_aliases())
                .all(|name| command.find_subcommand(name).is_none())
        })
        .cloned()
        .collect();
    for subcommand in additions {
        command = command.subcommand(subcommand);
    }
    command
}

fn local_main(arguments: Vec<OsString>) -> ExitCode {
    let _startup = startup_timing::Stage::new("process");
    match try_main(arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:?}");
            ExitCode::from(process_exit_code(&error))
        }
    }
}

fn try_main(arguments: Vec<OsString>) -> Result<()> {
    launcher::initialize_install_root();
    launcher::dispatch_update(&arguments)?;
    nanocodex::oai::transport::install_default_rustls_crypto_provider();
    // A menu observation must not select credentials from whichever project
    // directory happened to launch it. Other CLI commands retain their normal
    // development dotenv behavior.
    let hand_observation = arguments.get(1).is_some_and(|argument| argument == "hand")
        && matches!(
            arguments.get(2).and_then(|argument| argument.to_str()),
            Some("menu-status" | "status")
        );
    if !hand_observation {
        let _ = dotenvy::dotenv();
    }

    let cli = parse_cli(arguments);
    if let Some(Command::VmRunConfig(command)) = &cli.command {
        return command.run();
    }
    run_with_runtime(run(cli))
}

fn parse_cli(arguments: Vec<OsString>) -> Cli {
    use clap::{FromArgMatches, error::ErrorKind, parser::ValueSource};

    let mut command = with_foreign_commands(Cli::command(), &nanocodex2::command());
    let matches = command
        .try_get_matches_from_mut(arguments)
        .unwrap_or_else(|error| error.exit());
    // Global harness/auth flags apply on either side of a subcommand. Local
    // interactive flags must not be silently ignored by a subcommand's config.
    let misplaced = matches.subcommand_name().and_then(|_| {
        command
            .get_arguments()
            .find(|argument| {
                !argument.is_global_set()
                    && matches.value_source(argument.get_id().as_str())
                        == Some(ValueSource::CommandLine)
            })
            .map(|argument| {
                argument
                    .get_long()
                    .map_or_else(|| argument.get_id().to_string(), |long| format!("--{long}"))
            })
    });
    if let Some(name) = misplaced {
        command
            .error(
                ErrorKind::ArgumentConflict,
                format!("{name} must follow a subcommand that supports it, or be used in interactive mode"),
            )
            .exit();
    }
    Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit())
}

fn run_with_runtime(future: impl std::future::Future<Output = Result<()>>) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(future);
    // Application cleanup has completed. Optional MCP discovery can still own a
    // blocking DNS lookup, which Tokio cannot cancel. Foreground work and its
    // owned cleanup were awaited above; give no extra exit grace period to
    // these disposable background tasks.
    runtime.shutdown_background();
    result
}

fn process_exit_code(error: &eyre::Report) -> u8 {
    if error.downcast_ref::<RetryableProcessExit>().is_some() {
        RETRYABLE_EXIT_CODE
    } else {
        1
    }
}

async fn run(cli: Cli) -> Result<()> {
    // Interactive startup owns maintenance after its first editable frame.
    let observation = matches!(&cli.command, Some(Command::Hand(hand)) if hand.is_observation());
    if !observation
        && !matches!(
            &cli.command,
            None | Some(Command::Resume(_) | Command::Homes(_))
        )
    {
        if let Err(error) = update::prepare_legacy_nightly_bootstrap() {
            eprintln!("warning: failed to prepare the Nanocodex updater bootstrap: {error:#}");
        }
        if !matches!(&cli.command, Some(Command::Update(_)))
            && let Err(error) = update::ensure_default_automatic_updates()
        {
            eprintln!("Could not configure automatic updates: {error:#}");
        }
    }
    match cli.command {
        Some(Command::Install(command)) => command.run().await,
        Some(Command::Setup(command)) => command.run().await,
        Some(Command::Tui(command)) => command.run().await.map_err(Into::into),
        Some(Command::Computer(command)) => command.run().await.map_err(|error| eyre!(error)),
        Some(Command::Hand(command)) => command.run().await,
        Some(Command::Account(command)) => {
            if let Some(receipt) = command.run_with_receipt().await? {
                hand_login::connect_after_login(&receipt).await;
            }
            Ok(())
        }
        Some(Command::Auth(command)) => {
            // Credential commands act on an explicitly chosen family; the
            // credential-dependent session default must not redirect them.
            let family = if cli.agent.has_explicit_harness() {
                cli.agent.selected_harness()?
            } else {
                nanocodex::HarnessFamily::Codex
            };
            command.run(family, cli.agent.claude_auth).await
        }
        Some(Command::Login(command)) => command.run().await,
        Some(Command::Connect(command)) => command.run().await,
        Some(Command::Status(command)) => command.run().await,
        Some(Command::Logout(command)) => command.run().await,
        Some(Command::Cookies(command)) => command.run().await,
        #[cfg(feature = "tempo")]
        Some(Command::Credits(command)) => command.run().await,
        Some(Command::Eval(command)) => command.run().await,
        Some(Command::VmRunConfig(_)) => unreachable!("VMM commands run before Tokio starts"),
        Some(Command::Run(command)) => {
            let _observability = command.observability.install(false)?;
            command.run.run(command.agent, command.vm).await
        }
        Some(Command::ManagedServer(command)) => command.run().await,
        Some(Command::Rewind(command)) => command.run().await,
        Some(Command::Homes(command)) => command.run(),
        Some(Command::Resume(command)) => {
            let command = *command;
            let _observability = command.observability.install(true)?;
            use nanocodex2::tui::local::{agent::LocalLaunch, sessions as local_sessions};
            let codex_home = config::default_codex_home()?;
            let explicit = if command.agent.has_explicit_harness() {
                Some(command.agent.selected_harness()?)
            } else {
                None
            };
            let id = if command.from.is_some() || command.at.is_some() {
                resume_point(
                    &codex_home,
                    command.session,
                    command.from,
                    command.at.as_deref(),
                    &command.agent,
                    explicit,
                )
                .await?
            } else if let Some(id) = command.session {
                id
            } else {
                // One picker over the saved sessions of every harness, newest
                // first; an explicit harness narrows it to that family.
                let candidates = local_sessions::discover(&codex_home)
                    .await?
                    .into_iter()
                    .filter(|session| {
                        explicit.is_none_or(|family| {
                            local_sessions::Harness::from(family) == session.harness
                        })
                    })
                    .collect::<Vec<_>>();
                if candidates.is_empty() {
                    return Err(local_sessions::none_found(&codex_home));
                }
                let Some(selected) = local_sessions::select(&candidates).await? else {
                    return Ok(());
                };
                selected.id
            };
            // Fail before entering the terminal when the session cannot load.
            let session = sessions::load(&codex_home, &id).await?;
            let mut launch = LocalLaunch {
                args: command.agent.resume(session)?,
                vm: command.vm,
                replaceable: false,
                initial_prompt: command.prompt,
                initial_instruction: None,
                resume: None,
            };
            launch.args.prefer_codex_for_vm(&launch.vm);
            launch.args.validate_model_settings()?;
            nanocodex2::tui::run_local(launch)
                .await
                .map_err(|error| eyre!("{error}"))
        }
        Some(Command::Update(command)) => command.run().await,
        None => {
            let _observability = cli.observability.install(true)?;
            let mut agent = cli.agent;
            agent.prefer_codex_for_vm(&cli.vm);
            // Explicit unsupported model settings fail before the terminal starts.
            agent.validate_model_settings()?;
            let replaceable = agent.resumed().is_none();
            nanocodex2::tui::run_local(nanocodex2::tui::local::agent::LocalLaunch {
                args: agent,
                vm: cli.vm,
                replaceable,
                initial_prompt: cli.prompt,
                initial_instruction: None,
                resume: None,
            })
            .await
            .map_err(|error| eyre!("{error}"))
        }
    }
}

/// `resume --from ROLLOUT` / `resume ID --at TURN`: starts a new session from a
/// saved point and returns its ID; the source is never changed.
///
/// A durable session of either harness branches through the session catalog in
/// its own family. A rollout file, or a rollout-only Codex thread, is copied as
/// a new Codex thread.
async fn resume_point(
    codex_home: &Path,
    session: Option<String>,
    from: Option<PathBuf>,
    at: Option<&str>,
    agent: &AgentArgs,
    explicit: Option<nanocodex::HarnessFamily>,
) -> Result<String> {
    let point = rollout_fork::Point::parse(at)?;
    if let Some(id) = session.as_deref()
        && from.is_none()
        && let Ok((store, turns)) = sessions::turns(codex_home, id).await
    {
        let at = match &point {
            rollout_fork::Point::End => nanocodex_durability::BranchPoint::Latest,
            rollout_fork::Point::Turn(turn) => {
                nanocodex_durability::BranchPoint::Through(turn.clone())
            }
            rollout_fork::Point::Count(count) => {
                let turn = count
                    .checked_sub(1)
                    .and_then(|index| turns.get(index))
                    .ok_or_else(|| {
                        eyre!(
                            "session {id} has {} turns; --at {count} is out of range",
                            turns.len()
                        )
                    })?;
                nanocodex_durability::BranchPoint::Through(turn.id.clone())
            }
        };
        let workspace = agent
            .requested_workspace()
            .map(Path::canonicalize)
            .transpose()
            .wrap_err("failed to resolve the new session's workspace")?;
        let branched = sessions::branch(&store, id, at, workspace).await?;
        eprintln!(
            "Started {} session {} from {id}.",
            branched.family(),
            branched.id()
        );
        return Ok(branched.id().to_owned());
    }
    // Rollout files and rollout-only threads are Codex history.
    if explicit == Some(nanocodex::HarnessFamily::Claude) {
        return Err(eyre!(
            "--from copies a Codex rollout into a new Codex thread and cannot start a Claude \
             session; branch a stored Claude session with nanocodex resume ID --at TURN"
        ));
    }
    let source = match (from, session) {
        (Some(path), _) => path,
        (None, Some(id)) => RolloutConfig::new(codex_home)
            .load_session(&id)
            .wrap_err_with(|| format!("unknown session {id}"))?
            .rollout_path()
            .to_path_buf(),
        (None, None) => return Err(eyre!("--at needs a session ID or --from")),
    };
    let workspace = match agent.requested_workspace() {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir()?,
    }
    .canonicalize()
    .wrap_err("failed to resolve the new thread's workspace")?;
    let thread_id = rollout_fork::fork(&source, &point, codex_home, &workspace)?;
    eprintln!("Started Codex thread {thread_id} from {}.", source.display());
    Ok(thread_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_waits_for_foreground_cleanup_before_success_or_error() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        for fails in [false, true] {
            let cleaned = Arc::new(AtomicBool::new(false));
            let observed = Arc::clone(&cleaned);
            let result = run_with_runtime(async move {
                // The application future owns and awaits this cleanup, even on
                // its error path. Runtime background shutdown must follow it.
                let cleanup = tokio::spawn(async move {
                    tokio::task::yield_now().await;
                    observed.store(true, Ordering::SeqCst);
                });
                cleanup.await.unwrap();
                if fails {
                    Err(eyre!("synthetic runtime failure"))
                } else {
                    Ok(())
                }
            });
            assert!(cleaned.load(Ordering::SeqCst));
            assert_eq!(result.is_err(), fails);
        }
    }

    #[test]
    fn runtime_shutdown_does_not_wait_for_background_blocking_work() {
        use std::{
            sync::mpsc,
            time::{Duration, Instant},
        };

        for fails in [false, true] {
            let (release, blocked) = mpsc::channel();
            let (finished, completion) = mpsc::channel();
            let started = Instant::now();
            let result = run_with_runtime(async move {
                let (ready, received) = tokio::sync::oneshot::channel();
                drop(tokio::task::spawn_blocking(move || {
                    let _ = ready.send(());
                    let _ = blocked.recv_timeout(Duration::from_secs(5));
                    let _ = finished.send(());
                }));
                received.await.unwrap();
                if fails {
                    Err(eyre!("synthetic runtime failure"))
                } else {
                    Ok(())
                }
            });
            let elapsed = started.elapsed();
            // Release our synthetic blocking task even if the timing assertion fails.
            let _ = release.send(());
            completion.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(
                elapsed < Duration::from_secs(1),
                "shutdown took {elapsed:?}"
            );
            assert_eq!(result.is_err(), fails);
        }
    }

    #[test]
    fn cookie_commands_auto_detect_supported_browsers_for_an_exact_origin() {
        let cli = Cli::try_parse_from([
            "nanocodex",
            "cookies",
            "sync",
            "https://console.twilio.com",
            "--cookie-auth",
            "interactive",
        ])
        .unwrap();
        assert!(matches!(cli.command, Some(Command::Cookies(_))));
        for source in ["local", "vault", "both"] {
            let cli = Cli::try_parse_from([
                "nanocodex",
                "cookies",
                "list",
                "https://console.twilio.com",
                "--from",
                source,
            ])
            .unwrap();
            assert!(matches!(cli.command, Some(Command::Cookies(_))));
        }
        assert!(
            Cli::try_parse_from([
                "nanocodex",
                "cookies",
                "sync",
                "https://console.twilio.com/path",
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "nanocodex",
                "cookies",
                "sync",
                "https://console.twilio.com",
                "--cookies",
                "brave",
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "nanocodex",
                "cookies",
                "list",
                "https://console.twilio.com/path",
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "nanocodex",
                "cookies",
                "list",
                "https://console.twilio.com",
                "--from",
                "somewhere",
            ])
            .is_err()
        );
    }

    #[cfg(feature = "tempo")]
    #[test]
    fn tempo_flag_selects_the_tui_transport() {
        let cli = Cli::try_parse_from([
            "nanocodex",
            "--provider.tempo",
            "--provider.tempo.wallet-store",
            "/tmp/tempo-wallet.json",
        ])
        .unwrap();

        assert!(cli.command.is_none());
        assert!(cli.agent.uses_tempo());
        assert_eq!(
            cli.agent.responses_transport(),
            nanocodex::oai::transport::ResponsesTransport::Https
        );
    }

    #[cfg(feature = "tempo")]
    #[test]
    fn tempo_flag_selects_the_one_shot_transport() {
        let cli = Cli::try_parse_from([
            "nanocodex",
            "run",
            "reply with ok",
            "--provider.tempo",
            "--provider.tempo.wallet-store",
            "/tmp/tempo-wallet.json",
        ])
        .unwrap();

        let Some(Command::Run(command)) = cli.command else {
            unreachable!();
        };
        assert!(command.agent.uses_tempo());
        assert_eq!(
            command.agent.responses_transport(),
            nanocodex::oai::transport::ResponsesTransport::Https
        );
    }

    #[test]
    fn openai_provider_is_explicitly_selectable() {
        let cli = Cli::try_parse_from(["nanocodex", "--provider.openai", "--api-key", "test-key"])
            .unwrap();

        assert!(!cli.agent.uses_tempo());
        assert_eq!(
            cli.agent.responses_transport(),
            nanocodex::oai::transport::ResponsesTransport::WebSocket
        );
    }

    #[test]
    fn local_durability_testing_has_explicit_identity_and_store() {
        let cli = Cli::try_parse_from([
            "nanocodex",
            "run",
            "durable turn",
            "--local-durability",
            "/tmp/nanocodex-durability.sqlite",
            "--local-durability-state-id",
            "hammer-root",
            "--request-id",
            "turn-1",
            "--rollouts",
            "false",
        ])
        .unwrap();

        let Some(Command::Run(command)) = cli.command else {
            panic!("run command was not parsed");
        };
        assert!(command.run.uses_local_durability());

        let error = Cli::try_parse_from([
            "nanocodex",
            "run",
            "durable turn",
            "--local-durability-state-id",
            "orphaned-state",
        ])
        .err()
        .unwrap();
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }

    #[test]
    fn hosted_connectors_have_a_focused_top_level_command() {
        let cli = Cli::try_parse_from(["nanocodex", "connect", "github"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Connect(_))));

        let login = Cli::try_parse_from(["nanocodex", "login", "--no-open"]).unwrap();
        assert!(matches!(login.command, Some(Command::Login(_))));

        let connect = Cli::try_parse_from(["nanocodex", "connect", "github", "--no-open"]).unwrap();
        assert!(matches!(connect.command, Some(Command::Connect(_))));

        let multiple = Cli::try_parse_from([
            "nanocodex",
            "connect",
            "gmail",
            "gdrive",
            "github",
            "--no-open",
        ])
        .unwrap();
        assert!(matches!(multiple.command, Some(Command::Connect(_))));
        assert!(Cli::try_parse_from(["nanocodex", "connect"]).is_err());

        let chatgpt = Cli::try_parse_from(["nanocodex", "auth", "login", "--no-open"]).unwrap();
        assert!(matches!(chatgpt.command, Some(Command::Auth(_))));

        assert!(Cli::try_parse_from(["nanocodex", "login", "--github"]).is_err());
    }

    #[test]
    fn vm_tools_are_opt_in_for_tui_and_one_shot_runs() {
        let tui = Cli::try_parse_from(["nanocodex"]).unwrap();
        assert!(!tui.vm.is_enabled());

        let tui = Cli::try_parse_from([
            "nanocodex",
            "--vm",
            "/tmp/rootfs",
            "--vm-workspace",
            "/workspace",
        ])
        .unwrap();
        assert!(tui.vm.is_enabled());

        let run = Cli::try_parse_from(["nanocodex", "run", "reply with ok", "--vm", "/tmp/rootfs"])
            .unwrap();
        let Some(Command::Run(run)) = run.command else {
            panic!("run command was not parsed");
        };
        assert!(run.vm.is_enabled());
    }

    #[test]
    fn browser_and_cookie_selection_follow_platform_defaults() {
        let tui = Cli::try_parse_from(["nanocodex"]).unwrap();
        assert!(tui.agent.browser_enabled());
        assert!(tui.agent.uses_persistent_browser_profile());
        assert!(!tui.agent.copies_all_browser_cookies());
        #[cfg(target_os = "macos")]
        assert!(!tui.agent.uses_brave_browser());
        #[cfg(target_os = "macos")]
        assert!(tui.agent.uses_interactive_browser_cookie_authorization());

        let tui = Cli::try_parse_from(["nanocodex", "--browser"]).unwrap();
        assert!(tui.agent.browser_enabled());
        assert!(!tui.agent.uses_brave_browser());

        let brave = Cli::try_parse_from(["nanocodex", "--browser=brave"]).unwrap();
        assert!(brave.agent.browser_enabled());
        assert!(brave.agent.uses_brave_browser());

        let chromium = Cli::try_parse_from(["nanocodex", "--browser=chromium"]).unwrap();
        assert!(chromium.agent.browser_enabled());
        assert!(!chromium.agent.uses_brave_browser());

        let interactive = Cli::try_parse_from(["nanocodex", "--cookie-auth=interactive"]).unwrap();
        assert!(
            interactive
                .agent
                .uses_interactive_browser_cookie_authorization()
        );

        let host_passkeys = Cli::try_parse_from(["nanocodex", "--passkeys=host"]).unwrap();
        assert!(host_passkeys.agent.uses_host_browser_passkeys());

        let temporary = Cli::try_parse_from(["nanocodex", "--browser-profile=temporary"]).unwrap();
        assert!(!temporary.agent.uses_persistent_browser_profile());
        assert!(temporary.agent.copies_all_browser_cookies());

        assert!(Cli::try_parse_from(["nanocodex", "--cookies=none"]).is_err());
        assert!(Cli::try_parse_from(["nanocodex", "--cookies=brave"]).is_err());

        let run = Cli::try_parse_from(["nanocodex", "run", "inspect example.com"]).unwrap();
        let Some(Command::Run(run)) = run.command else {
            panic!("run command was not parsed");
        };
        assert!(run.agent.browser_enabled());

        let disabled = Cli::try_parse_from(["nanocodex", "--browser=none"]).unwrap();
        assert!(!disabled.agent.browser_enabled());
        assert!(!disabled.agent.copies_all_browser_cookies());
    }

    #[test]
    fn vm_tuning_requires_an_opted_in_rootfs() {
        let error = Cli::try_parse_from(["nanocodex", "--vm-cpus", "4"])
            .err()
            .unwrap();

        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }

    #[cfg(feature = "tempo")]
    #[test]
    fn provider_selection_is_exclusive() {
        let error = Cli::try_parse_from(["nanocodex", "--provider.openai", "--provider.tempo"])
            .err()
            .unwrap();

        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[cfg(not(feature = "tempo"))]
    #[test]
    fn tempo_provider_is_absent_from_direct_agent_builds() {
        let error = Cli::try_parse_from(["nanocodex", "--provider.tempo"])
            .err()
            .unwrap();

        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn resume_accepts_a_thread_id_and_agent_configuration() {
        let cli = Cli::try_parse_from([
            "nanocodex",
            "resume",
            "019c0d31-c308-7d91-bff4-5dca82d15ac6",
            "--provider.openai",
            "--api-key",
            "test-key",
            "--prompt",
            "continue",
        ])
        .unwrap();

        let Some(Command::Resume(command)) = cli.command else {
            panic!("resume command was not parsed");
        };
        assert_eq!(
            command.session.as_deref(),
            Some("019c0d31-c308-7d91-bff4-5dca82d15ac6")
        );
        assert_eq!(command.prompt.as_deref(), Some("continue"));
        assert!(!command.agent.uses_tempo());
    }

    #[test]
    fn resume_without_a_thread_id_opens_discovery_path() {
        let cli = Cli::try_parse_from(["nanocodex", "resume", "--provider.openai"])
            .expect("resume should accept an omitted thread UUID");

        let Some(Command::Resume(command)) = cli.command else {
            panic!("resume command was not parsed");
        };
        assert!(command.session.is_none());
    }
}
