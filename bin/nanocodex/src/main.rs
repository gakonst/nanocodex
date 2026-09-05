mod auth;
mod benchmark;
mod browser;
mod browser_cookie_sync;
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
mod login;
mod managed_memory;
mod managed_server;
mod mcp;
#[cfg_attr(not(feature = "tempo"), path = "mpp_disabled.rs")]
mod mpp;
mod observability;
mod run;
mod subagents;
mod tui;
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

use std::{path::PathBuf, process::ExitCode};

use clap::{Args, Parser, Subcommand, builder::NonEmptyStringValueParser};
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
    version = version::SHORT_VERSION,
    long_version = version::LONG_VERSION,
    about = "An interactive coding agent and headless JSONL runner",
    args_conflicts_with_subcommands = true,
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
    /// Manage `ChatGPT` subscription login.
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
    /// Resume a Codex or Nanocodex thread in the interactive TUI.
    Resume(Box<ResumeCommand>),
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
    /// Codex thread UUID to resume. Omit it to select from discovered sessions.
    #[arg(value_parser = NonEmptyStringValueParser::new())]
    thread_id: Option<String>,

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

fn main() -> ExitCode {
    match try_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:?}");
            ExitCode::from(process_exit_code(&error))
        }
    }
}

fn try_main() -> Result<()> {
    nanocodex::oai::transport::install_default_rustls_crypto_provider();
    // Keep direct `cargo run` behavior consistent with the Justfile without
    // requiring shell-specific syntax to load the repository's `.env` file.
    let _ = dotenvy::dotenv();

    if let Err(error) = update::prepare_legacy_nightly_bootstrap() {
        eprintln!("warning: failed to prepare the Nanocodex updater bootstrap: {error:#}");
    }

    let cli = Cli::parse();
    if let Some(Command::VmRunConfig(command)) = &cli.command {
        return command.run();
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(cli))
}

fn process_exit_code(error: &eyre::Report) -> u8 {
    if error.downcast_ref::<RetryableProcessExit>().is_some() {
        RETRYABLE_EXIT_CODE
    } else {
        1
    }
}

async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Some(Command::Auth(command)) => command.run().await,
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
            let _observability = command.observability.install(false, command.agent.cwd())?;
            command.run.run(command.agent, command.vm).await
        }
        Some(Command::ManagedServer(command)) => command.run().await,
        Some(Command::Resume(command)) => {
            let codex_home = config::default_codex_home()?;
            let rollouts = RolloutConfig::new(&codex_home);
            let thread_id = match command.thread_id {
                Some(thread_id) => thread_id,
                None => {
                    let sessions = rollouts.list_sessions().wrap_err_with(|| {
                        format!(
                            "failed to discover Codex threads under {}",
                            codex_home.display()
                        )
                    })?;
                    if sessions.is_empty() {
                        return Err(eyre!(
                            "no resumable Codex threads found under {}",
                            codex_home.display()
                        ));
                    }
                    let Some(thread_id) = tui::select_resume_session(&sessions)? else {
                        return Ok(());
                    };
                    thread_id
                }
            };
            let session = rollouts
                .load_session(&thread_id)
                .wrap_err_with(|| format!("failed to load Codex thread {thread_id}"))?;
            let workspace = PathBuf::from(session.workspace());
            let _observability = command.observability.install(true, &workspace)?;
            tui::run(
                command.agent,
                command.vm,
                command.prompt.map(tui::InitialPrompt::plain),
                Some(session),
            )
            .await
        }
        Some(Command::Update(command)) => command.run().await,
        None => {
            let _observability = cli.observability.install(true, cli.agent.cwd())?;
            tui::run(
                cli.agent,
                cli.vm,
                cli.prompt.map(tui::InitialPrompt::plain),
                None,
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            command.thread_id.as_deref(),
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
        assert!(command.thread_id.is_none());
    }
}
