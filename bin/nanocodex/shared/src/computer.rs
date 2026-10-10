//! Installation management shared by both native CLIs.
use clap::{Args, Subcommand};
use std::{fs, path::PathBuf, process::Stdio};

#[derive(Args)]
pub struct Computer {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Install and select OpenAI's signed headless CUA components.
    Setup {
        /// Check OpenAI's component feed and update when its signed build changed.
        #[arg(long)]
        refresh: bool,
        /// Internal background preparation; coalesce concurrent startup requests.
        #[arg(long, hide = true)]
        background: bool,
    },
}

impl Computer {
    pub async fn run(self) -> Result<(), String> {
        let Command::Setup {
            refresh,
            background,
        } = self.command;
        if cfg!(target_os = "linux") {
            // Linux Hands capture and control through their built-in native
            // screen; OpenAI's signed component feed is macOS-only. Report what
            // the native screen needs instead of a provider failure. Read-only:
            // nothing is downloaded, installed, or recorded as a setup failure.
            println!("{}", linux_native_screen_receipt());
            return Ok(());
        }
        let _background_lock = if background {
            let directory = setup_directory()?;
            fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
            let mut options = fs::OpenOptions::new();
            options.create(true).read(true).write(true).truncate(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600).custom_flags(nix::libc::O_NOFOLLOW);
            }
            let lock = options
                .open(directory.join("background.lock"))
                .map_err(|error| error.to_string())?;
            match lock.try_lock() {
                Ok(()) => Some(lock),
                Err(std::fs::TryLockError::WouldBlock) => return Ok(()),
                Err(error) => return Err(error.to_string()),
            }
        } else {
            None
        };
        if background {
            eprintln!("Preparing signed Computer Use components…");
        }
        // This attempt supersedes the previous outcome; running Hands report
        // preparing until it records its own.
        let directory = setup_directory()?;
        clear_setup_failure(&directory)?;
        let result = async {
            let receipt = nanocodex_computer::provision::provision_upstream(refresh).await?;
            // Discover the exact provider catalog off the interactive path, so a
            // later attachment can register it from the version-bound cache.
            if receipt["status"] == "installed" {
                eprintln!("Components verified; preparing the Computer Use tool catalog…");
                let config = nanocodex_computer::provision::config_from_receipt(&receipt)?;
                nanocodex_computer::ComputerTools::connect(config)
                    .await
                    .map_err(|error| error.to_string())?;
                eprintln!(
                    "Computer Use components are ready; running Hands discover them on their next computer call."
                );
            }
            Ok::<_, String>(receipt)
        }
        .await;
        match result {
            Ok(receipt) => {
                if receipt["status"] == "installed" {
                    clear_setup_failure(&directory)?;
                } else {
                    record_setup_failure(&directory, &receipt)?;
                }
                println!("{receipt}");
                Ok(())
            }
            Err(error) => {
                record_setup_failure(
                    &directory,
                    &serde_json::json!({"status": "failed", "error": error}),
                )?;
                Err(error)
            }
        }
    }
}

/// Prerequisites of the Hand's private X11 desktop, found on this PATH. Presence
/// is not proof the screen works: the running Hand reports that itself through
/// `nanocodex hand permissions --check`. The Wayland helpers are embedded in
/// the Hand executable and extracted on first use; nothing is downloaded.
fn linux_native_screen_receipt() -> serde_json::Value {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut found = serde_json::Map::new();
    let mut missing = Vec::new();
    for name in ["Xvfb", "openbox", "xterm", "ffmpeg"] {
        let executable = std::env::split_paths(&path)
            .map(|directory| directory.join(name))
            .find(|candidate| {
                use std::os::unix::fs::PermissionsExt as _;
                fs::metadata(candidate).is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
            });
        if executable.is_none() {
            missing.push(name);
        }
        found.insert(
            name.into(),
            executable.map(|path| path.display().to_string()).into(),
        );
    }
    serde_json::json!({
        "platform": "linux",
        "provider": "native_screen",
        "status": if missing.is_empty() { "prerequisites_found" } else { "prerequisites_missing" },
        "prerequisites": found,
        "missing": missing,
        "note": "A same-user Wayland session needs none of these; the Hand otherwise runs its own private Xvfb desktop (fonts are also required). Nothing was downloaded or installed.",
        "next": if missing.is_empty() {
            "nanocodex hand permissions --check".to_owned()
        } else {
            format!("install {} with your system package manager, then run: nanocodex hand permissions --check", missing.join(", "))
        },
    })
}

/// Outcome of the last setup attempt that did not install the components,
/// read by running Hands so a failed background installation is reported
/// instead of an indefinite "preparing".
const SETUP_FAILURE: &str = "setup-failure.json";

fn clear_setup_failure(directory: &std::path::Path) -> Result<(), String> {
    match fs::remove_file(directory.join(SETUP_FAILURE)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "could not clear the previous Computer Use setup outcome: {error}"
        )),
    }
}

fn record_setup_failure(
    directory: &std::path::Path,
    receipt: &serde_json::Value,
) -> Result<(), String> {
    fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let mut failure = receipt.clone();
    failure["log"] = directory.join("setup.log").display().to_string().into();
    failure["retry"] = "nanocodex computer setup".into();
    // Readers see the previous complete outcome or this one, never a torn file.
    let staged = directory.join(format!(".{SETUP_FAILURE}.{}", std::process::id()));
    fs::write(&staged, failure.to_string()).map_err(|error| error.to_string())?;
    fs::rename(&staged, directory.join(SETUP_FAILURE)).map_err(|error| error.to_string())
}

#[allow(dead_code)] // Read by the Hand gateway only.
fn setup_failure(directory: Option<&std::path::Path>) -> Option<serde_json::Value> {
    let path = directory?.join(SETUP_FAILURE);
    if fs::metadata(&path).ok()?.len() > 64 * 1024 {
        return None;
    }
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// Connect the Hand before optional CUA downloads. The child owns provisioning
/// (including its cross-process lock) and survives installer exit. Explicit
/// provider selections, including `off`, never trigger a download.
#[allow(dead_code)] // Also compiled into the managed CLI.
pub fn setup_in_background(refresh: bool) -> Result<Option<PathBuf>, String> {
    if !cfg!(target_os = "macos")
        || std::env::var_os("NANOCODEX_COMPUTER").is_some_and(|value| !value.is_empty())
    {
        return Ok(None);
    }
    let directory = setup_directory()?;
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let log_path = directory.join("setup.log");
    let mut options = fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600).custom_flags(nix::libc::O_NOFOLLOW);
    }
    let log = options.open(&log_path).map_err(|error| error.to_string())?;
    if !log.metadata().map_err(|error| error.to_string())?.is_file() {
        return Err("Computer Use setup log must be a regular file".into());
    }
    let mut command =
        std::process::Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
    command.args(["computer", "setup", "--background"]);
    if refresh {
        command.arg("--refresh");
    }
    command
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|error| error.to_string())?)
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    // Reap while the caller stays open. Exiting the caller does not terminate
    // the independently owned installer.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(Some(log_path))
}

fn setup_directory() -> Result<PathBuf, String> {
    let base = std::env::var_os("NANOCODEX_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".nanocodex")))
        .ok_or("HOME or NANOCODEX_DIR is required for Computer Use setup")?;
    Ok(base.join("runtimes/openai-cua"))
}

/// Optional managed CUA may not hold shell, file access, or first input behind
/// MCP startup. Explicit custom providers retain their normal error contract.
pub async fn connect_for_startup() -> Result<Option<nanocodex_computer::ComputerTools>, String> {
    if let Err(error) = setup_in_background(false) {
        tracing::warn!(%error, "could not start background Computer Use setup");
    }
    let Some(config) = nanocodex_computer::ComputerConfig::discover() else {
        return Ok(None);
    };
    if std::env::var_os("NANOCODEX_COMPUTER").is_some_and(|value| !value.is_empty()) {
        return nanocodex_computer::ComputerTools::connect(config)
            .await
            .map(Some)
            .map_err(|error| error.to_string());
    }
    match tokio::time::timeout(
        std::time::Duration::from_millis(500),
        nanocodex_computer::ComputerTools::connect(config),
    )
    .await
    {
        Ok(Ok(computer)) => Ok(Some(computer)),
        Ok(Err(error)) => {
            tracing::warn!(%error, "optional Computer Use provider unavailable; continuing startup");
            Ok(None)
        }
        Err(_) => {
            tracing::info!(
                "Computer Use is still preparing; continuing startup with native Hand controls"
            );
            Ok(None)
        }
    }
}

/// Stable Hand catalog: provisioning may finish after the publisher connects.
/// Resolve the provider on use, retaining it once connected so existing realms
/// and workspace processes survive. No action is retried after dispatch.
#[allow(dead_code)] // Shared with the CLI installer, which does not publish tools.
pub async fn connect_for_hand() -> Result<Option<nanocodex_computer::ComputerTools>, String> {
    if std::env::var_os("NANOCODEX_COMPUTER").is_some_and(|value| !value.is_empty()) {
        return connect_for_startup().await;
    }
    // Linux keeps its working native screen when no managed provider is selected.
    // macOS provisions signed components asynchronously after Hand startup.
    if !cfg!(target_os = "macos") && nanocodex_computer::ComputerConfig::discover().is_none() {
        return Ok(None);
    }
    if let Err(error) = setup_in_background(false) {
        tracing::warn!(%error, "could not start background Computer Use setup");
    }
    use serde_json::json;
    let catalog = ["js", "js_reset"].into_iter().map(|name| {
        serde_json::from_value(json!({
            "name": name,
            "description": "NANOCODEX_DYNAMIC_CUA_V1. Persistent Computer Use gateway. Call with no arguments first to discover the live provider's exact tools, schemas and instructions. Then pass its js/js_reset arguments directly, or use provider_tool and arguments for another model-visible provider tool. Components may still be preparing; a later call discovers their completed installation without restarting the Hand. Catalog availability does not prove screen capture or input permission.",
            "inputSchema": {"type":"object", "additionalProperties":true}
        })).expect("static gateway catalog")
    }).collect();
    Ok(Some(nanocodex_computer::ComputerTools::new(
        LazyComputer {
            state: tokio::sync::Mutex::new(LazyComputerState::default()),
            setup: setup_directory().ok(),
        },
        catalog,
    )))
}

#[allow(dead_code)]
struct LazyComputer {
    state: tokio::sync::Mutex<LazyComputerState>,
    /// Background setup state directory, consulted while no provider exists.
    setup: Option<PathBuf>,
}

#[allow(dead_code)] // Also compiled into the installer, which does not publish tools.
#[derive(Default)]
struct LazyComputerState {
    connected: Option<nanocodex_computer::ComputerTools>,
    initializing:
        Option<tokio::task::JoinHandle<Result<nanocodex_computer::ComputerTools, String>>>,
}

impl Drop for LazyComputer {
    fn drop(&mut self) {
        if let Some(initializing) = self.state.get_mut().initializing.take() {
            initializing.abort();
        }
    }
}

#[async_trait::async_trait]
impl nanocodex_computer::ComputerExecutor for LazyComputer {
    async fn end_turn(
        &self,
        session: &str,
        turn: &str,
        event: &str,
    ) -> Result<(), nanocodex::oai::tools::ToolError> {
        let computer = self.state.lock().await.connected.clone();
        if let Some(computer) = computer {
            computer.end_turn(session, turn, event).await?;
        }
        Ok(())
    }

    async fn invoke_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
        context: nanocodex::oai::tools::ToolContext<'_>,
    ) -> nanocodex::oai::tools::ToolResult {
        use nanocodex::oai::tools::{Tool as _, ToolInput};
        use serde_json::json;
        let discovery =
            name == "js" && arguments.as_object().is_some_and(serde_json::Map::is_empty);
        let computer = {
            let mut state = self.state.lock().await;
            if state.connected.is_none() {
                if state.initializing.is_none() {
                    let Some(config) = nanocodex_computer::ComputerConfig::discover() else {
                        let failure = setup_failure(self.setup.as_deref());
                        if discovery {
                            return discovery_output(
                                failure.unwrap_or_else(|| json!({"status":"preparing"})),
                            );
                        }
                        if let Some(failure) = failure {
                            return Err(format!("Computer Use setup did not install the components; no action was dispatched: {failure}").into());
                        }
                        return Err("Computer Use components are unavailable; discover the Hand contract again before sending input".into());
                    };
                    // Retain initialization across discovery deadlines and caller
                    // cancellation. The provider owns its 120-second startup bound;
                    // restarting it every five seconds can starve cold startup.
                    state.initializing = Some(tokio::spawn(async move {
                        nanocodex_computer::ComputerTools::connect(config)
                            .await
                            .map_err(|error| error.to_string())
                    }));
                }
                let initializing = state.initializing.as_mut().expect("started above");
                match tokio::time::timeout(std::time::Duration::from_secs(5), initializing).await {
                    Ok(result) => {
                        state.initializing = None;
                        let computer = result.map_err(|error| format!("Computer Use initialization task failed: {error}"))??;
                        state.connected = Some(computer);
                    }
                    Err(_) if discovery => return discovery_output(json!({"status":"preparing"})),
                    Err(_) => return Err("Computer Use is still initializing; no action was dispatched. Discover the Hand contract again before sending input".into()),
                }
            }
            state.connected.as_ref().expect("connected above").clone()
        };
        if discovery {
            let catalog = computer
                .tools()
                .map(|tool| {
                    let mut definition = serde_json::to_value(tool.definition())
                        .expect("serializable provider tool definition");
                    definition["name"] = json!(tool.provider_definition().name);
                    definition
                })
                .collect::<Vec<_>>();
            return discovery_output(json!({"status":"ready", "definitions":catalog}));
        }
        let (name, arguments) = match arguments.get("provider_tool") {
            Some(tool) => (
                tool.as_str().ok_or("provider_tool must be a string")?,
                arguments
                    .get("arguments")
                    .cloned()
                    .ok_or("arguments is required with provider_tool")?,
            ),
            None => (name, arguments.clone()),
        };
        let tool = computer.tool(name).filter(|tool| tool.provider_definition().model_visible())
            .ok_or("The selected provider does not expose this model-visible tool; discover its catalog first")?;
        tool.execute(
            ToolInput::Function(serde_json::value::to_raw_value(&arguments)?),
            context,
        )
        .await
    }
}

#[allow(dead_code)]
fn discovery_output(value: serde_json::Value) -> nanocodex::oai::tools::ToolResult {
    nanocodex_computer::output(
        serde_json::json!({"content":[{"type":"text", "text":value.to_string()}]}),
    )
}

#[cfg(all(test, unix))]
mod lazy_gateway_journey {
    use super::*;
    use nanocodex::oai::tools::ToolContext;
    use nanocodex_computer::{ComputerConfig, ComputerExecutor, ComputerTools};
    use serde_json::json;

    // The external MCP provider is the only synthetic dependency. Exercise the
    // actual Hand gateway, its deadline, and the retained stdio initialization.
    #[tokio::test]
    async fn slow_catalog_survives_discovery_deadline_and_dispatches_once() {
        let directory = tempfile::tempdir().unwrap();
        let provider = directory.path().join("provider.py");
        let calls = directory.path().join("calls.jsonl");
        let ready = directory.path().join("ready");
        let starts = directory.path().join("starts");
        fs::write(&provider, r#"import json,sys,time,os
for line in sys.stdin:
    request=json.loads(line)
    if 'id' not in request: continue
    method=request['method']
    if method == 'initialize':
        with open(sys.argv[3], 'a') as f: f.write('start\n')
        while not os.path.exists(sys.argv[2]): time.sleep(0.01)
        result={'protocolVersion':'2025-06-18','capabilities':{},'serverInfo':{'name':'fixture','version':'1'}}
    elif method == 'tools/list':
        result={'tools':[{'name':n,'description':n,'inputSchema':{'type':'object'}} for n in ['js','js_reset']]}
    else:
        with open(sys.argv[1], 'a') as f: f.write(json.dumps(request)+'\n')
        result={'content':[{'type':'text','text':'fixture action complete'}]}
    print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}),flush=True)
"#).unwrap();
        let mut config = ComputerConfig::mcp("/usr/bin/python3");
        config.args = vec![
            provider.into_os_string(),
            calls.clone().into_os_string(),
            ready.clone().into_os_string(),
            starts.clone().into_os_string(),
        ];
        let gateway = LazyComputer {
            state: tokio::sync::Mutex::new(LazyComputerState {
                connected: None,
                initializing: Some(tokio::spawn(async move {
                    ComputerTools::connect(config)
                        .await
                        .map_err(|e| e.to_string())
                })),
            }),
            setup: None,
        };
        let context = |call| ToolContext::new("lazy-gateway", "fixture-session", call, &[], 16000);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(50),
                gateway.invoke_tool("js", json!({}), context("cancelled")),
            )
            .await
            .is_err()
        );
        let first = gateway
            .invoke_tool("js", json!({}), context("first"))
            .await
            .unwrap();
        eprintln!(
            "after cancelled discovery, expected preparing: {}",
            first.structured_result()
        );
        assert!(first.structured_result().to_string().contains("preparing"));
        assert!(!calls.exists(), "discovery must not dispatch an action");
        fs::write(ready, "ready").unwrap();
        let next = gateway
            .invoke_tool("js", json!({}), context("second"))
            .await
            .unwrap();
        eprintln!(
            "after releasing provider, expected ready: {}",
            next.structured_result()
        );
        assert!(next.structured_result().to_string().contains("ready"));
        assert_eq!(
            fs::read_to_string(starts).unwrap().lines().count(),
            1,
            "startup must not restart after discovery timed out"
        );
        let action = gateway
            .invoke_tool("js", json!({"code":"fixture"}), context("action"))
            .await
            .unwrap();
        eprintln!(
            "explicit action, expected success: {}",
            action.structured_result()
        );
        assert!(action.success);
        assert_eq!(fs::read_to_string(calls).unwrap().lines().count(), 1);
    }

    #[tokio::test]
    async fn failed_initialization_surfaces_error_instead_of_preparing() {
        let gateway = LazyComputer {
            state: tokio::sync::Mutex::new(LazyComputerState {
                connected: None,
                initializing: Some(tokio::spawn(async {
                    ComputerTools::connect(ComputerConfig::mcp("/nonexistent/cua-provider"))
                        .await
                        .map_err(|e| e.to_string())
                })),
            }),
            setup: None,
        };
        let error = gateway
            .invoke_tool(
                "js",
                json!({}),
                ToolContext::new("lazy-gateway", "fixture-session", "failure", &[], 16000),
            )
            .await
            .err()
            .expect("initialization failure must be visible");
        eprintln!("expected provider startup error: {error}");
        assert!(
            error
                .to_string()
                .contains("Cannot start upstream Sky MCP provider")
        );
    }
}
