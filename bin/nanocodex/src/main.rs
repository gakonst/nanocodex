mod auth;
mod browser;
mod config;
#[cfg(feature = "tempo")]
mod credits;
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

use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand, builder::NonEmptyStringValueParser};
use eyre::{Result, WrapErr, eyre};
use nanocodex::agent::rollout::{DurableSession, DurableSessionSummary, RolloutConfig};

use config::AgentArgs;
use observability::ObservabilityArgs;

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
    /// Inspect or purchase Nanocodex NANOUSD credits.
    #[cfg(feature = "tempo")]
    Credits(credits::Credits),
    /// Internal entrypoint for one dedicated libkrun VMM process.
    #[command(hide = true)]
    VmRunConfig(vm::VmRunConfig),
    /// Run one prompt and stream JSONL events to stdout.
    Run(Box<RunCommand>),
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
    /// Codex thread UUID to resume. Omit it to choose from sessions interactively.
    #[arg(value_parser = NonEmptyStringValueParser::new())]
    thread_id: Option<String>,

    /// Show sessions from every workspace instead of only the current repository.
    #[arg(long, conflicts_with = "thread_id")]
    all: bool,

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

fn main() -> Result<()> {
    nanocodex::oai::transport::install_default_rustls_crypto_provider();
    // Keep direct `cargo run` behavior consistent with the Justfile without
    // requiring shell-specific syntax to load the repository's `.env` file.
    let _ = dotenvy::dotenv();

    let cli = Cli::parse();
    if let Some(Command::VmRunConfig(command)) = &cli.command {
        return command.run();
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(cli))
}

async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Some(Command::Auth(command)) => command.run().await,
        #[cfg(feature = "tempo")]
        Some(Command::Credits(command)) => command.run().await,
        Some(Command::VmRunConfig(_)) => unreachable!("VMM commands run before Tokio starts"),
        Some(Command::Run(command)) => {
            let _observability = command.observability.install(false, command.agent.cwd())?;
            command.run.run(command.agent, command.vm).await
        }
        Some(Command::Resume(command)) => {
            let codex_home = config::default_codex_home()?;
            let rollout = RolloutConfig::new(&codex_home);
            let session = if let Some(thread_id) = command.thread_id.as_deref() {
                rollout
                    .load_session(thread_id)
                    .wrap_err_with(|| format!("failed to load Codex thread {thread_id}"))?
            } else {
                let current =
                    command.agent.cwd().canonicalize().wrap_err(
                        "failed to resolve the current workspace for session discovery",
                    )?;
                let scope = repository_scope(&current);
                let sessions = rollout
                    .list_sessions()
                    .wrap_err("failed to list resumable Codex sessions")?
                    .into_iter()
                    .filter(|session| command.all || session_matches_scope(session, &scope))
                    .collect::<Vec<_>>();
                if sessions.is_empty() {
                    let hint = if command.all {
                        String::new()
                    } else {
                        "; use `nanocodex resume --all` to search every workspace".to_owned()
                    };
                    return Err(eyre!(
                        "no resumable sessions found for {}{hint}",
                        scope.display()
                    ));
                }
                let Some(thread_id) = tui::select_session(&sessions, command.all).await? else {
                    return Ok(());
                };
                rollout
                    .load_session(&thread_id)
                    .wrap_err_with(|| format!("failed to load Codex thread {thread_id}"))?
            };
            let workspace = PathBuf::from(session.workspace());
            let _observability = command.observability.install(true, &workspace)?;
            run_tui_session_loop(
                command.agent,
                command.vm,
                command.prompt,
                Some(session),
                rollout,
            )
            .await
        }
        Some(Command::Update(command)) => command.run().await,
        None => {
            let _observability = cli.observability.install(true, cli.agent.cwd())?;
            let codex_home = config::default_codex_home()?;
            run_tui_session_loop(
                cli.agent,
                cli.vm,
                cli.prompt,
                None,
                RolloutConfig::new(&codex_home),
            )
            .await
        }
    }
}

async fn run_tui_session_loop(
    agent: AgentArgs,
    vm: vm::VmArgs,
    initial_prompt: Option<String>,
    initial_session: Option<DurableSession>,
    rollout: RolloutConfig,
) -> Result<()> {
    let mut prompt = initial_prompt;
    let mut session = initial_session;
    loop {
        match tui::run(agent.clone(), vm.clone(), prompt.take(), session.take()).await? {
            tui::TuiExit::Quit { session_id } => {
                println!("{}", tui::resume_hint(&session_id));
                return Ok(());
            }
            tui::TuiExit::Resume {
                session_id,
                workspace,
            } => {
                let scope = repository_scope(&workspace);
                let sessions = rollout
                    .list_sessions()
                    .wrap_err("failed to list resumable Codex sessions")?
                    .into_iter()
                    .filter(|candidate| session_matches_scope(candidate, &scope))
                    .collect::<Vec<_>>();
                let selected = if sessions.is_empty() {
                    None
                } else {
                    tui::select_session(&sessions, false).await?
                };
                let selected = selected.as_deref().unwrap_or(&session_id);
                session = Some(
                    rollout
                        .load_session(selected)
                        .wrap_err_with(|| format!("failed to load Codex thread {selected}"))?,
                );
            }
        }
    }
}

fn repository_scope(workspace: &Path) -> PathBuf {
    workspace
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .unwrap_or(workspace)
        .to_path_buf()
}

fn session_matches_scope(session: &DurableSessionSummary, scope: &Path) -> bool {
    session
        .workspace()
        .canonicalize()
        .is_ok_and(|workspace| repository_scope(&workspace) == scope)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn browser_tool_is_opt_in_for_tui_and_one_shot_runs() {
        let tui = Cli::try_parse_from(["nanocodex"]).unwrap();
        assert!(!tui.agent.browser_enabled());

        let tui = Cli::try_parse_from(["nanocodex", "--browser"]).unwrap();
        assert!(tui.agent.browser_enabled());

        let brave =
            Cli::try_parse_from(["nanocodex", "--browser=brave", "--cookies=true"]).unwrap();
        assert!(brave.agent.browser_enabled());

        let chromium = Cli::try_parse_from(["nanocodex", "--browser", "--cookies=true"]).unwrap();
        assert!(chromium.agent.browser_enabled());

        let chrome_source =
            Cli::try_parse_from(["nanocodex", "--browser=brave", "--cookies=chrome"]).unwrap();
        assert!(chrome_source.agent.browser_enabled());

        let firefox_source =
            Cli::try_parse_from(["nanocodex", "--browser", "--cookies=firefox"]).unwrap();
        assert!(firefox_source.agent.browser_enabled());

        let safari_source =
            Cli::try_parse_from(["nanocodex", "--browser", "--cookies=safari"]).unwrap();
        assert!(safari_source.agent.browser_enabled());

        let run =
            Cli::try_parse_from(["nanocodex", "run", "inspect example.com", "--browser"]).unwrap();
        let Some(Command::Run(run)) = run.command else {
            panic!("run command was not parsed");
        };
        assert!(run.agent.browser_enabled());
    }

    #[test]
    fn browser_cookies_require_an_opted_in_browser() {
        let error = Cli::try_parse_from(["nanocodex", "--cookies=true"])
            .err()
            .unwrap();

        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }

    #[test]
    fn browser_executable_requires_the_opt_in() {
        let error = Cli::try_parse_from([
            "nanocodex",
            "--browser-executable",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
        ])
        .err()
        .unwrap();

        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
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
    fn resume_without_a_thread_id_opens_the_workspace_picker() {
        let cli = Cli::try_parse_from(["nanocodex", "resume", "--all"]).unwrap();

        let Some(Command::Resume(command)) = cli.command else {
            panic!("resume command was not parsed");
        };
        assert!(command.thread_id.is_none());
        assert!(command.all);
    }

    #[test]
    fn resume_all_conflicts_with_an_explicit_thread_id() {
        let error = Cli::try_parse_from([
            "nanocodex",
            "resume",
            "019c0d31-c308-7d91-bff4-5dca82d15ac6",
            "--all",
        ])
        .err()
        .unwrap();

        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn repository_scope_uses_the_nearest_git_ancestor() {
        let directory = tempfile::tempdir().unwrap();
        let repository = directory.path().join("repository");
        let nested = repository.join("crates/example");
        std::fs::create_dir_all(repository.join(".git")).unwrap();
        std::fs::create_dir_all(&nested).unwrap();

        assert_eq!(repository_scope(&nested), repository);
    }
}
