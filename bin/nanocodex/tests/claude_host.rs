//! Shipped-CLI journeys against synthetic Messages/SSE. No live inference or
//! credentials. Artifacts retain every provider request and CLI stdout/stderr.
#[path = "support/local_cli.rs"]
mod local_cli;
use local_cli::local_cli;

#[path = "support/claude_code_fixture.rs"]
mod code_fixture;
use axum::{Json, Router, routing::post};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpListener, process::Command};

fn sse(block: Value) -> impl axum::response::IntoResponse {
    let tool = block["type"] == "tool_use";
    let start = if tool {
        json!({"type":"tool_use","id":block["id"],"name":block["name"],"input":{}})
    } else {
        json!({"type":"text","text":""})
    };
    let delta = if tool {
        json!({"type":"input_json_delta","partial_json":block["input"].to_string()})
    } else {
        json!({"type":"text_delta","text":block["text"]})
    };
    let body: String = [json!({"type":"message_start","message":{"id":"fixture","role":"assistant","model":"claude-sonnet-5-5","content":[],"usage":{"input_tokens":1,"output_tokens":0}}}),
    json!({"type":"content_block_start","index":0,"content_block":start}), json!({"type":"content_block_delta","index":0,"delta":delta}), json!({"type":"content_block_stop","index":0}), json!({"type":"message_delta","delta":{"stop_reason":if tool {"tool_use"} else {"end_turn"}},"usage":{"output_tokens":1}}),json!({"type":"message_stop"})].iter().map(|v|format!("data: {v}\n\n")).collect();
    ([("content-type", "text/event-stream")], body)
}
fn tool(stage: usize, name: &str, input: Value) -> Value {
    code_fixture::tool(format!("call-{stage}"), name, input)
}
fn result(body: &Value) -> Option<&Value> {
    body["messages"]
        .as_array()?
        .iter()
        .rev()
        .filter_map(|m| m["content"].as_array())
        .flatten()
        .find(|block| block["type"] == "tool_result")
}
fn parsed_result(body: &Value) -> Value {
    let result = result(body).unwrap();
    let text = result["content"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| {
            result["content"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|b| b["text"].as_str())
                .collect()
        });
    serde_json::from_str(&text).unwrap_or(json!(text))
}
fn command(workspace: &Path, endpoint: &str) -> Command {
    let mut cmd = Command::new(local_cli());
    cmd.arg("run")
        .current_dir(workspace)
        .env_clear()
        .env("HOME", workspace.join("home"))
        .env("CODEX_HOME", workspace.join("codex-home"))
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("NANOCODEX_COMPUTER", "off")
        .args([
            "--claude",
            "--model",
            "claude-sonnet-5-5",
            "--thinking",
            "medium",
            "--claude-api-key",
            "synthetic-fixture-key",
            "--claude-messages-url",
            endpoint,
            "--rollouts",
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
            "--cwd",
        ])
        .arg(workspace)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd
}
fn artifact(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../output/claude-host")
        .join(format!("{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(path.join("workspace/home")).unwrap();
    path
}
#[tokio::test]
async fn native_cli_tasks_and_agent_lifecycle() {
    let artifact = artifact("native-host");
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let root_count = Arc::new(Mutex::new(0usize));
    let log = requests.clone();
    let count = root_count.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let app = Router::new().route("/v1/messages",post(move |Json(body): Json<Value>| {let body = code_fixture::normalize(body);
        let log = log.clone(); let count = count.clone();
        async move {
            let first = body["messages"][0]["content"].to_string();
            let child = first.contains("HOST_CHILD_FIXTURE");
            log.lock().unwrap().push(json!({"child":child,"request":body}));
            let reply = if child {
                if first.contains("HOST_CHILD_SLOW") { tokio::time::sleep(Duration::from_secs(30)).await; }
                if result(&body).is_none() { tool(100,"submit_result",json!({"output":"child-complete"})) }
                else { json!({"type":"text","text":"child finished"}) }
            } else {
                let stage = { let mut n = count.lock().unwrap(); let current = *n; *n += 1; current };
                match stage {
                    0 => tool(stage,"TaskCreate",json!({"subject":"Real CLI task","description":"Complete the native journey"})),
                    1 => tool(stage,"TaskUpdate",json!({"taskId":"1","status":"completed"})),
                    2 => tool(stage,"TaskGet",json!({"taskId":"1"})),
                    3 => tool(stage,"TodoWrite",json!({"todos":[{"content":"Verify native host","activeForm":"Verifying native host","status":"completed"}]})),
                    4 => tool(stage,"spawn_agent",json!({"role":"CLI child","task":"HOST_CHILD_FIXTURE return child-complete","output_contract":{"kind":"string"}})),
                    5 => tool(stage,"wait_agent",json!({"agent_ids":[1],"timeout_ms":20000})),
                    6 => tool(stage,"list_agents",json!({"include_completed":true})),
                    7 => tool(stage,"send_agent_message",json!({"agent_id":1,"message":"HOST_CHILD_FIXTURE follow-up"})),
                    8 => tool(stage,"spawn_agent",json!({"role":"second child","task":"HOST_CHILD_FIXTURE return child-complete","output_contract":{"kind":"string"}})),
                    9 => tool(stage,"wait_agent",json!({"agent_ids":[2],"timeout_ms":20000})),
                    10 => tool(stage,"spawn_agent",json!({"role":"unsupported family","task":"never execute","harness":"invalid-other","output_contract":{"kind":"string"}})),
                    11 => tool(stage,"spawn_agent",json!({"role":"interruptible child","task":"HOST_CHILD_FIXTURE HOST_CHILD_SLOW wait for cancellation","output_contract":{"kind":"string"}})),
                    12 => tool(stage,"interrupt_agent",json!({"agent_id":3})),
                    13 => tool(stage,"wait_agent",json!({"agent_ids":[3],"timeout_ms":1})),
                    14 => tool(stage,"send_agent_message",json!({"agent_id":1,"message":"HOST_CHILD_FIXTURE resume prompt","purpose":"delegate"})),
                    _ => json!({"type":"text","text":"native-host-journey-complete"}),
                }
            };
            sse(reply)
        }
    }));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut cmd = command(&artifact.join("workspace"), &endpoint);
    cmd.arg("Exercise native host tools.");
    std::fs::write(artifact.join("scenario.txt"),format!("Command: cargo +1.97.0 test -p nanocodex-bin --test claude_host native_cli_tasks_and_agent_lifecycle\nCLI: {cmd:?}\nExpected: durable-board task CRUD, actual child spawn/submit/wait/message/interrupt and unsupported harness errors. Shared shell execution and continuation are covered by claude-code-mode-cli-journey.py and claude-native-cli-journey.py.\n")).unwrap();
    let output = tokio::time::timeout(Duration::from_secs(70), cmd.output())
        .await
        .expect("CLI journey timed out")
        .unwrap();
    std::fs::write(artifact.join("stdout.txt"), &output.stdout).unwrap();
    std::fs::write(artifact.join("stderr.txt"), &output.stderr).unwrap();
    let requests = requests.lock().unwrap();
    std::fs::write(
        artifact.join("requests.json"),
        serde_json::to_vec_pretty(&*requests).unwrap(),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}: {}",
        artifact.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("native-host-journey-complete"));
    let roots: Vec<_> = requests
        .iter()
        .filter(|r| r["child"] == false)
        .map(|r| &r["request"])
        .collect();
    assert_eq!(roots.len(), 16, "inspect {}", artifact.display());
    let names: Vec<_> = roots[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert_eq!(names, vec!["exec", "wait"]);
    assert_eq!(result(roots[11]).unwrap()["is_error"], true);
    assert_eq!(parsed_result(roots[1])["task"]["id"], "1");
    assert_eq!(parsed_result(roots[3])["task"]["status"], "completed");
    assert_eq!(parsed_result(roots[5])["agent_id"], 1);
    assert!(
        parsed_result(roots[6])
            .to_string()
            .contains("child-complete"),
        "{}",
        parsed_result(roots[6])
    );
    assert!(parsed_result(roots[7]).to_string().contains("CLI child"));
    assert_ne!(result(roots[8]).unwrap()["is_error"], true);
    assert!(
        parsed_result(roots[10])
            .to_string()
            .contains("child-complete")
    );
    assert_eq!(parsed_result(roots[12])["agent_id"], 3);
    assert_ne!(result(roots[13]).unwrap()["is_error"], true);
    assert!(
        parsed_result(roots[14]).to_string().contains("interrupted"),
        "{}",
        parsed_result(roots[14])
    );
    assert_eq!(parsed_result(roots[15])["to_agent_id"], 1);
    assert_ne!(result(roots[15]).unwrap()["is_error"], true);
    std::fs::write(
        artifact.join("outcome.txt"),
        "PASS: shipped CLI native host lifecycle and failure journey; synthetic provider only.\n",
    )
    .unwrap();
    eprintln!("evidence: {}", artifact.display());
    server.abort();
}

#[test]
fn native_cli_durable_files_media_and_retained_shell_sessions() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .arg(repository.join("scripts/tests/claude-native-cli-journey.py"))
        .args(["--binary", local_cli()])
        .current_dir(&repository)
        .output()
        .expect("run native CLI durability journey");
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
