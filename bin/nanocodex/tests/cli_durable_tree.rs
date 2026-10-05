//! Shipped CLI durability journeys over both providers' actual HTTP transports.
//! Only provider inference is synthetic. Traces and CLI receipts live in output/.
use axum::{Json, Router, routing::post};
use eyre::{Result, eyre};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::{Output, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::AsyncReadExt as _, net::TcpListener, process::Command, sync::Notify, time::timeout,
};

const DEADLINE: Duration = Duration::from_secs(45);
const ROOT: &str = "synthetic-cli-root";
const CODEX: &str = "gpt-6.1-sol";
const CLAUDE: &str = "claude-sonnet-5-5";

struct Fixture {
    base: String,
    artifact: PathBuf,
    requests: Arc<Mutex<Vec<Value>>>,
    paused: Arc<Notify>,
    parent_finished: Arc<Notify>,
    release: Arc<Notify>,
    recovering: Arc<AtomicBool>,
    server: tokio::task::JoinHandle<()>,
}

fn result(body: &Value) -> Value {
    if let Some(messages) = body["messages"].as_array() {
        messages
            .iter()
            .rev()
            .filter_map(|message| message["content"].as_array())
            .flat_map(|blocks| blocks.iter().rev())
            .find(|block| block["type"] == "tool_result")
            .map(|block| block["content"].clone())
            .unwrap_or(Value::Null)
    } else {
        body["input"]
            .as_array()
            .into_iter()
            .flatten()
            .rev()
            .find(|item| item["type"] == "custom_tool_call_output")
            .map(|item| item["output"].clone())
            .unwrap_or(Value::Null)
    }
}

fn user_text(body: &Value) -> String {
    body.get("input")
        .unwrap_or(&body["messages"])
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["role"] == "user")
        .flat_map(|item| match &item["content"] {
            Value::String(text) => vec![text.clone()],
            Value::Array(blocks) => blocks
                .iter()
                .filter(|block| block["type"] == "text" || block["type"] == "input_text")
                .filter_map(|block| block["text"].as_str().map(str::to_owned))
                .collect(),
            _ => vec![],
        })
        .collect::<Vec<_>>()
        .join("\n")
}

enum Reply {
    Text(&'static str),
    Code(String),
    Bash,
}
impl Reply {
    fn stream(&self, family: &str, id: usize) -> String {
        let call = format!("fixture-call-{id}");
        if family == "codex" {
            let output = match self {
                Self::Text(text) => {
                    json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]})
                }
                Self::Code(code) => {
                    json!({"type":"custom_tool_call","name":"exec","call_id":call,"input":code})
                }
                Self::Bash => unreachable!(),
            };
            return format!(
                "data: {}\n\ndata: [DONE]\n\n",
                json!({"type":"response.completed","response":{"id":format!("response-{id}"),"status":"completed","output":[output],"usage":{"input_tokens":1,"input_tokens_details":{"cached_tokens":0},"output_tokens":1,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":2}}})
            );
        }
        let (block, delta, stop) = match self {
            Self::Text(text) => (
                json!({"type":"text","text":""}),
                json!({"type":"text_delta","text":text}),
                "end_turn",
            ),
            Self::Code(code) => (
                json!({"type":"tool_use","id":call,"name":"exec","input":{}}),
                json!({"type":"input_json_delta","partial_json":json!({"code":code}).to_string()}),
                "tool_use",
            ),
            Self::Bash => (
                json!({"type":"tool_use","id":call,"name":"Bash","input":{}}),
                json!({"type":"input_json_delta","partial_json":json!({"command":"printf x >> effect.log"}).to_string()}),
                "tool_use",
            ),
        };
        [json!({"type":"message_start","message":{"id":format!("message-{id}"),"type":"message","role":"assistant","model":CLAUDE,"content":[],"usage":{"input_tokens":1,"output_tokens":0}}}),
            json!({"type":"content_block_start","index":0,"content_block":block}),
            json!({"type":"content_block_delta","index":0,"delta":delta}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":1}}),
            json!({"type":"message_stop"})].iter().map(|event| format!("data: {event}\n\n")).collect()
    }
}

impl Fixture {
    async fn start(name: &str, cold: bool) -> Result<Self> {
        let artifact = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../output/cli-durable-tree")
            .join(format!("{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(artifact.join("workspace/home"))?;
        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let paused = Arc::new(Notify::new());
        let parent_finished = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let recovering = Arc::new(AtomicBool::new(false));
        let handler = post({
            let requests = Arc::clone(&requests);
            let paused = Arc::clone(&paused);
            let parent_finished = Arc::clone(&parent_finished);
            let release = Arc::clone(&release);
            let recovering = Arc::clone(&recovering);
            let artifact = artifact.clone();
            move |Json(body): Json<Value>| {
                let requests = Arc::clone(&requests);
                let paused = Arc::clone(&paused);
                let parent_finished = Arc::clone(&parent_finished);
                let release = Arc::clone(&release);
                let recovering = Arc::clone(&recovering);
                let artifact = artifact.clone();
                async move {
                    let family = if body["model"].as_str().unwrap().starts_with("claude") {
                        "claude"
                    } else {
                        "codex"
                    };
                    let prompt = user_text(&body);
                    let child = prompt.contains("COLD_CHILD");
                    let last = result(&body);
                    let id = {
                        let mut requests = requests.lock().unwrap();
                        requests.push(json!({"family":family,"child":child,"recovery":recovering.load(Ordering::SeqCst),"request":body,"tool_result":last}));
                        std::fs::write(
                            artifact.join("http-transcript.json"),
                            serde_json::to_vec_pretty(&*requests).unwrap(),
                        )
                        .unwrap();
                        requests.len()
                    };
                    let reply = if !cold {
                        Reply::Text("durable-http-answer")
                    } else if child {
                        if last.is_null() {
                            if family == "claude" {
                                Reply::Bash
                            } else {
                                Reply::Code("text(await tools.exec_command({cmd:'printf x >> effect.log',shell:'/bin/sh',login:false}));".into())
                            }
                        } else if last.to_string().contains("accepted") {
                            Reply::Text("child-finished")
                        } else {
                            if !recovering.load(Ordering::SeqCst) {
                                paused.notify_one();
                                release.notified().await;
                            }
                            Reply::Code("const receipt=await tools.submit_result({output:'durable-child-result'}); text(receipt); if(!receipt.accepted) throw Error('child submission rejected');".into())
                        }
                    } else if last.is_null() {
                        let other = if family == "codex" { "claude" } else { "codex" };
                        let model = if other == "claude" { CLAUDE } else { CODEX };
                        Reply::Code(format!(
                            "text(await tools.spawn_agent({{harness:'{other}',model:'{model}',thinking:'medium',role:'cold child',task:'COLD_CHILD',lifetime:'foreground',output_contract:{{kind:'string'}}}}));"
                        ))
                    } else {
                        parent_finished.notify_one();
                        Reply::Text("owned-root-answer")
                    };
                    (
                        [("content-type", "text/event-stream")],
                        reply.stream(family, id),
                    )
                }
            }
        });
        let router = Router::new()
            .route("/responses", handler.clone())
            .route("/v1/messages", handler);
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        println!("CLI_HTTP_EVIDENCE {}", artifact.display());
        Ok(Self {
            base,
            artifact,
            requests,
            paused,
            parent_finished,
            release,
            recovering,
            server,
        })
    }

    fn command(
        &self,
        family: &str,
        request: &str,
        prompt: &str,
        default_rollouts: bool,
    ) -> Command {
        let workspace = self.artifact.join("workspace");
        let mut command = Command::new(env!("CARGO_BIN_EXE_nanocodex"));
        command
            .current_dir(&workspace)
            .env_clear()
            .env("HOME", workspace.join("home"))
            .env("CODEX_HOME", workspace.join("codex-home"))
            .env("PATH", "/usr/bin:/bin")
            .env("NANOCODEX_COMPUTER", "off")
            .args([
                "run",
                "--harness",
                family,
                "--model",
                if family == "claude" { CLAUDE } else { CODEX },
                "--thinking",
                "medium",
                "--api-key",
                "synthetic-openai-key",
                "--claude-api-key",
                "synthetic-claude-key",
                "--api-base-url",
                &self.base,
                "--claude-messages-url",
                &format!("{}/v1/messages", self.base),
                "--responses-transport",
                "https",
                "--websocket-warmup",
                "false",
                "--store-responses",
                "false",
                "--browser=none",
                "--mcp-defaults",
                "false",
                "--mcp-codex-config",
                "false",
                "--web-search",
                "false",
                "--image-generation",
                "false",
                "--memory",
                "false",
                "--subagents",
                "true",
            ])
            .arg("--cwd")
            .arg(&workspace)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if !default_rollouts {
            command
                .args(["--rollouts", "false", "--local-durability"])
                .arg(workspace.join("durability.sqlite"))
                .args(["--local-durability-state-id", ROOT, "--request-id", request]);
        }
        command.arg(prompt);
        command
    }

    async fn run(&self, mut command: Command, name: &str) -> Result<Output> {
        std::fs::write(
            self.artifact.join(format!("{name}-command.txt")),
            format!("{command:?}\n"),
        )?;
        let output = timeout(DEADLINE, command.output())
            .await
            .map_err(|_| eyre!("CLI deadline exceeded: {name}"))??;
        evidence(&self.artifact, name, &output)?;
        Ok(output)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn evidence(artifact: &Path, name: &str, output: &Output) -> Result<()> {
    std::fs::write(
        artifact.join(format!("{name}-stdout.jsonl")),
        &output.stdout,
    )?;
    std::fs::write(artifact.join(format!("{name}-stderr.txt")), &output.stderr)?;
    std::fs::write(
        artifact.join(format!("{name}-outcome.json")),
        serde_json::to_vec_pretty(
            &json!({"success":output.status.success(),"exit_code":output.status.code()}),
        )?,
    )?;
    Ok(())
}
fn success(output: &Output, expected: &str) {
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(expected),
        "missing answer {expected}: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}
fn events(output: &Output) -> Result<Vec<Value>> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}

fn completed_receipt(output: &Output) -> Result<Value> {
    success(output, "run.completed");
    let terminals: Vec<_> = events(output)?
        .into_iter()
        .filter(|event| event["type"] == "run.completed")
        .collect();
    assert_eq!(terminals.len(), 1, "expected exactly one terminal receipt");
    assert_eq!(terminals[0]["payload"]["status"], "completed");
    assert!(
        terminals[0]["request_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    Ok(terminals.into_iter().next().unwrap())
}

fn replay_receipt(output: &Output) -> Result<Value> {
    let terminal = completed_receipt(output)?;
    // Native Claude also acknowledges prompt admission before its replay receipt.
    assert!(
        events(output)?
            .iter()
            .all(|event| { event["type"] == "input.accepted" || event["type"] == "run.completed" })
    );
    assert_eq!(terminal["payload"]["model_calls"], 0);
    Ok(terminal)
}

#[tokio::test]
async fn shipped_cli_http_roots_replay_and_restore_history_for_both_families() -> Result<()> {
    for family in ["codex", "claude"] {
        let fixture = Fixture::start(family, false).await?;
        let first = fixture
            .run(
                fixture.command(family, "first-turn", "FIRST_HTTP_TURN", false),
                "initial",
            )
            .await?;
        success(&first, "durable-http-answer");
        let count = fixture.requests.lock().unwrap().len();
        let replay = fixture
            .run(
                fixture.command(family, "first-turn", "FIRST_HTTP_TURN", false),
                "replay",
            )
            .await?;
        // Replay retains the native root identity without dispatching inference.
        let initial_receipt = completed_receipt(&first)?;
        assert_eq!(
            replay_receipt(&replay)?["request_id"],
            initial_receipt["request_id"]
        );
        assert_eq!(
            fixture.requests.lock().unwrap().len(),
            count,
            "terminal replay dispatched inference"
        );
        let next = fixture
            .run(
                fixture.command(family, "next-turn", "NEXT_HTTP_TURN", false),
                "follow-on",
            )
            .await?;
        success(&next, "durable-http-answer");
        assert_eq!(
            completed_receipt(&next)?["request_id"],
            initial_receipt["request_id"]
        );
        let requests = fixture.requests.lock().unwrap();
        assert_eq!(requests.len(), count + 1);
        let restored = requests.last().unwrap()["request"].to_string();
        assert!(
            restored.contains("FIRST_HTTP_TURN")
                && restored.contains("durable-http-answer")
                && restored.contains("NEXT_HTTP_TURN"),
            "cold root lost committed history: {restored}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn shipped_cli_default_claude_rollouts_start_with_owned_durability() -> Result<()> {
    let fixture = Fixture::start("claude-default-rollouts", false).await?;
    let first = fixture
        .run(
            fixture.command("claude", "unused", "DEFAULT_CLAUDE_ROLLOUT", true),
            "default-rollouts",
        )
        .await?;
    success(&first, "durable-http-answer");
    completed_receipt(&first)?;
    assert!(
        fixture
            .artifact
            .join("workspace/codex-home/claude/sessions.sqlite")
            .exists()
    );
    Ok(())
}

async fn cold_child(family: &str) -> Result<()> {
    let fixture = Fixture::start(&format!("{family}-cold-child"), true).await?;
    let mut command = fixture.command(family, "cold-turn", "SPAWN_OWNED_CHILD", false);
    std::fs::write(
        fixture.artifact.join("killed-command.txt"),
        format!("{command:?}\n"),
    )?;
    let mut child = command.spawn()?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let stdout = tokio::spawn(async move {
        let mut bytes = vec![];
        stdout.read_to_end(&mut bytes).await.map(|_| bytes)
    });
    let stderr = tokio::spawn(async move {
        let mut bytes = vec![];
        stderr.read_to_end(&mut bytes).await.map(|_| bytes)
    });
    timeout(DEADLINE, async {
        fixture.paused.notified().await;
        fixture.parent_finished.notified().await;
    })
    .await
    .map_err(|_| {
        eyre!(
            "did not reach committed child effect + root ownership barrier; evidence {}",
            fixture.artifact.display()
        )
    })?;
    assert!(
        child.try_wait()?.is_none(),
        "root exited while foreground child still needed completion"
    );
    let effect = fixture.artifact.join("workspace/effect.log");
    assert_eq!(std::fs::read_to_string(&effect)?, "x");
    child.kill().await?;
    let status = child.wait().await?;
    let killed = Output {
        status,
        stdout: stdout.await??,
        stderr: stderr.await??,
    };
    evidence(&fixture.artifact, "killed", &killed)?;
    let killed_events = events(&killed)?;
    assert!(
        !killed_events
            .iter()
            .any(|event| event["type"] == "run.completed"),
        "root exposed completion before its foreground child settled"
    );
    fixture.recovering.store(true, Ordering::SeqCst);
    fixture.release.notify_one();
    let resumed = fixture
        .run(
            fixture.command(family, "cold-turn", "SPAWN_OWNED_CHILD", false),
            "cold-recovery",
        )
        .await?;
    // Recovery can publish a retained terminal receipt without re-emitting
    // an answer journaled before the process was killed at the ownership barrier.
    let recovered_receipt = completed_receipt(&resumed)?;
    assert_eq!(
        recovered_receipt["request_id"],
        killed_events[0]["request_id"]
    );
    assert_eq!(
        std::fs::read_to_string(&effect)?,
        "x",
        "cold child repeated its committed append"
    );
    let other = if family == "claude" {
        "codex"
    } else {
        "claude"
    };
    let recovered_request_count = {
        let requests = fixture.requests.lock().unwrap();
        let child_recovery: Vec<_> = requests
            .iter()
            .filter(|record| record["child"] == true && record["recovery"] == true)
            .collect();
        assert!(
            !child_recovery.is_empty(),
            "new process did not reconstruct the unfinished child"
        );
        assert!(
            child_recovery
                .iter()
                .all(|record| !record["tool_result"].is_null()),
            "cold child lost its committed tool history"
        );
        assert!(
            child_recovery
                .iter()
                .all(|record| record["family"] == other),
            "cold reconstruction changed harness family"
        );
        assert!(
            child_recovery
                .iter()
                .flat_map(|record| record["tool_result"].as_array().into_iter().flatten())
                .filter_map(|block| block["text"].as_str())
                .filter_map(|text| serde_json::from_str::<Value>(text).ok())
                .any(|receipt| receipt["accepted"] == true && receipt["status"] == "accepted"),
            "recovered child never received an accepted structured result receipt"
        );
        requests.len()
    };
    let replay = fixture
        .run(
            fixture.command(family, "cold-turn", "SPAWN_OWNED_CHILD", false),
            "completed-cold-replay",
        )
        .await?;
    assert_eq!(
        replay_receipt(&replay)?["request_id"],
        recovered_receipt["request_id"]
    );
    assert_eq!(
        fixture.requests.lock().unwrap().len(),
        recovered_request_count,
        "completed cold tree replay dispatched root or child inference"
    );
    assert_eq!(std::fs::read_to_string(&effect)?, "x");
    let database = fixture.artifact.join("workspace/durability.sqlite");
    let connection = rusqlite::Connection::open(database)?;
    let states: Vec<String> = connection
        .prepare("SELECT state_id FROM nanocodex_durable_states WHERE state_id LIKE ?1")?
        .query_map([format!("{ROOT}/child/%")], |row| row.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    assert_eq!(
        states.len(),
        1,
        "each child must have its own durable execution state: {states:?}"
    );
    std::fs::write(
        fixture.artifact.join("recovery-contract.json"),
        serde_json::to_vec_pretty(&json!({
            "expected":"root terminal withheld until foreground child completes; process kill, mixed-family cold reconstruction, committed append exactly once, independent child execution state",
            "observed":{"root_family":family,"child_family":other,"effect":"x","child_state_ids":states,"terminal_after_recovery":true,"native_root_session_id":recovered_receipt["request_id"],"accepted_child_result":true,"completed_replay_inference_calls":0},
            "reproduce":"cargo +1.97 test -p nanocodex-bin --test cli_durable_tree -- --nocapture"
        }))?,
    )?;
    Ok(())
}

#[tokio::test]
async fn shipped_cli_codex_root_cold_recovers_its_owned_claude_child() -> Result<()> {
    cold_child("codex").await
}
#[tokio::test]
async fn shipped_cli_claude_root_cold_recovers_its_owned_codex_child() -> Result<()> {
    cold_child("claude").await
}
