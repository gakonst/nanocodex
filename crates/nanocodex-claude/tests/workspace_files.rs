#![cfg(feature = "tools")]

use axum::{Json, Router, routing::post};
use nanocodex_agent::Nanocodex;
use nanocodex_claude::{Claude, ClaudeClient};
use nanocodex_claude_tools::ClaudeWorkspaceFiles;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

fn sse(block: Value, stop: &str) -> String {
    let mut out = String::new();
    let mut emit = |v: Value| out.push_str(&format!("data: {v}\n\n"));
    emit(
        json!({"type":"message_start","message":{"id":"msg","role":"assistant","model":"test","content":[],"usage":{"input_tokens":2,"output_tokens":0}}}),
    );
    emit(
        json!({"type":"content_block_start","index":0,"content_block":if block["type"]=="text" { json!({"type":"text","text":""}) } else {json!({"type":"tool_use","id":block["id"],"name":block["name"],"input":{}})}}),
    );
    emit(
        json!({"type":"content_block_delta","index":0,"delta":if block["type"]=="text" { json!({"type":"text_delta","text":block["text"]}) } else { json!({"type":"input_json_delta","partial_json":block["input"].to_string()}) }}),
    );
    emit(json!({"type":"content_block_stop","index":0}));
    emit(json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":4}}));
    emit(json!({"type":"message_stop"}));
    out
}

#[tokio::test]
async fn explicitly_opted_in_claude_tools_never_expose_codex_catalog() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tempfile::tempdir().unwrap();
    let files = Arc::new(ClaudeWorkspaceFiles::new(dir.path()).unwrap());
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = requests.clone();
    let app = Router::new().route("/v1/messages",post(move |Json(body):Json<Value>| {
        let log=log.clone();
        async move {
            let index={let mut l=log.lock().unwrap(); l.push(body); l.len()};
            let (block,reason)=match index {
                1 => (json!({"type":"tool_use","id":"w1","name":"Write","input":{"file_path":"sample.txt","content":"hello Claude\n"}}),"tool_use"),
                2 => (json!({"type":"tool_use","id":"r1","name":"Read","input":{"file_path":"sample.txt"}}),"tool_use"),
                _ => (json!({"type":"text","text":"finished"}),"end_turn"),
            };
            ([("content-type", "text/event-stream")], sse(block,reason))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    );
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .workspace_files(files)
        .build()
        .unwrap();
    assert_eq!(
        agent
            .prompt("create and read a file")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "finished"
    );
    let log = requests.lock().unwrap();
    assert_eq!(log.len(), 3);
    let names: Vec<_> = log[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Read", "Edit", "Write", "Glob", "Grep"]);
    assert_eq!(
        log[1]["messages"][2]["content"][0]["content"],
        "Wrote sample.txt"
    );
    assert_eq!(
        log[2]["messages"][4]["content"][0]["content"],
        "1\thello Claude\n"
    );
    server.abort();
}

#[tokio::test]
async fn completed_file_write_survives_rejected_followup_in_session() {
    use axum::{http::StatusCode, response::IntoResponse};
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tempfile::tempdir().unwrap();
    let files = Arc::new(ClaudeWorkspaceFiles::new(dir.path()).unwrap());
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = requests.clone();
    let app = Router::new().route("/v1/messages", post(move |Json(body):Json<Value>| {
        let log=log.clone();
        async move {
            let index={let mut l=log.lock().unwrap();l.push(body);l.len()};
            match index {
                1 => ([ ("content-type","text/event-stream") ],sse(json!({"type":"tool_use","id":"w1","name":"Write","input":{"file_path":"effect.txt","content":"written once"}}),"tool_use")).into_response(),
                2 => (StatusCode::BAD_REQUEST,"synthetic follow-up rejection").into_response(),
                _ => ([ ("content-type","text/event-stream") ],sse(json!({"type":"text","text":"resumed"}),"end_turn")).into_response(),
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{addr}/v1/messages"),
        "synthetic",
    );
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .workspace_files(files)
        .build()
        .unwrap();
    assert!(agent.prompt("write").await.unwrap().result().await.is_err());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("effect.txt")).unwrap(),
        "written once"
    );
    assert_eq!(
        agent
            .prompt("continue")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "resumed"
    );
    let r = requests.lock().unwrap();
    assert_eq!(r.len(), 3);
    assert_eq!(r[2]["messages"][1]["content"][0]["name"], "Write");
    assert_eq!(r[2]["messages"][2]["content"][0]["tool_use_id"], "w1");
    server.abort();
}

#[tokio::test]
async fn opt_in_tasks_notebook_and_sandbox_bash_route_without_host_shell() {
    use nanocodex_claude_tools::{
        bash::{BashRequest, BashResult, ClaudeBash, SandboxBashExecutor},
        notebook::ClaudeNotebook,
        tasks::ClaudeTasks,
    };
    struct FakeSandbox;
    impl SandboxBashExecutor for FakeSandbox {
        async fn execute(&self, request: BashRequest) -> Result<BashResult, String> {
            assert_eq!(request.command, "printf safe");
            Ok(BashResult {
                stdout: "safe".into(),
                stderr: String::new(),
                exit_code: 0,
                truncated: false,
            })
        }
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("demo.ipynb"),serde_json::to_vec(&json!({"nbformat":4,"cells":[{"id":"a","cell_type":"markdown","source":["old"],"metadata":{}}]})).unwrap()).unwrap();
    let tasks = Arc::new(ClaudeTasks::new());
    let notebook = Arc::new(ClaudeNotebook::new(dir.path()).unwrap());
    let bash = Arc::new(ClaudeBash::new(FakeSandbox));
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = requests.clone();
    let app=Router::new().route("/v1/messages",post(move |Json(body):Json<Value>| {
        let log=log.clone();
        async move {
            let idx={let mut r=log.lock().unwrap();r.push(body);r.len()};
            let (block,stop)=match idx {
                1 => (json!({"type":"tool_use","id":"t1","name":"TaskCreate","input":{"subject":"Write notes","description":"Use notebook"}}),"tool_use"),
                2 => (json!({"type":"tool_use","id":"n1","name":"NotebookEdit","input":{"notebook_path":"demo.ipynb","new_source":"new","cell_id":"a"}}),"tool_use"),
                3 => (json!({"type":"tool_use","id":"b1","name":"Bash","input":{"command":"printf safe"}}),"tool_use"),
                _ => (json!({"type":"text","text":"complete"}),"end_turn"),
            };
            ([ ("content-type","text/event-stream") ],sse(block,stop))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    );
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .tasks(tasks)
        .notebook(notebook)
        .sandbox_bash(bash)
        .build()
        .unwrap();
    assert_eq!(
        agent
            .prompt("test tools")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "complete"
    );
    let log = requests.lock().unwrap();
    let names: Vec<_> = log[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"TaskCreate") && names.contains(&"NotebookEdit") && names.contains(&"Bash")
    );
    assert!(!names.contains(&"exec_command") && !names.contains(&"apply_patch"));
    assert_eq!(log[3]["messages"][6]["content"][0]["tool_use_id"], "b1");
    assert_eq!(
        serde_json::from_str::<Value>(
            log[3]["messages"][6]["content"][0]["content"]
                .as_str()
                .unwrap()
        )
        .unwrap()["stdout"],
        "safe"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(dir.path().join("demo.ipynb")).unwrap())
            .unwrap()["cells"][0]["source"],
        json!(["new"])
    );
    server.abort();
}

#[tokio::test]
async fn native_workspace_read_media_and_scoped_context_reach_messages_transport() {
    use base64::Engine as _;
    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGMQ0QgAAADgAI3uVJhMAAAAAElFTkSuQmCC";
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/CLAUDE.md"),
        "Use the scoped chart conventions.\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("src/chart.png"),
        base64::engine::general_purpose::STANDARD
            .decode(PNG)
            .unwrap(),
    )
    .unwrap();
    std::fs::write(dir.path().join("src/bad.png"), "broken image").unwrap();
    std::fs::write(dir.path().join("src/demo.ipynb"), serde_json::to_vec(&json!({"nbformat":4,"cells":[
        {"id":"plot","cell_type":"code","metadata":{},"source":["plot()"],"execution_count":1,"outputs":[{"output_type":"display_data","metadata":{},"data":{"image/png":PNG,"text/plain":"One chart"}}]}
    ]})).unwrap()).unwrap();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = requests.clone();
    let app = Router::new().route("/v1/messages", post(move |Json(body): Json<Value>| {
        let log = log.clone();
        async move {
            let index = { let mut log = log.lock().unwrap(); log.push(body); log.len() };
            let (block, stop) = match index {
                1 => (json!({"type":"tool_use","id":"image","name":"Read","input":{"file_path":"src/chart.png"}}), "tool_use"),
                2 => (json!({"type":"tool_use","id":"notebook","name":"Read","input":{"file_path":"src/demo.ipynb"}}), "tool_use"),
                3 => (json!({"type":"tool_use","id":"broken","name":"Read","input":{"file_path":"src/bad.png"}}), "tool_use"),
                _ => (json!({"type":"text","text":"Native media received; malformed input reported."}), "end_turn"),
            };
            ([("content-type", "text/event-stream")], sse(block, stop))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    );
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .workspace_files(Arc::new(ClaudeWorkspaceFiles::new(dir.path()).unwrap()))
        .build()
        .unwrap();
    assert_eq!(
        agent
            .prompt("Read a chart and its notebook, then inspect malformed media.")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "Native media received; malformed input reported."
    );
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    let image = &requests[1]["messages"][2]["content"][0];
    assert_eq!(image["tool_use_id"], "image");
    assert_eq!(
        image["content"][0],
        json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":PNG}})
    );
    assert!(
        image["content"][1]["text"]
            .as_str()
            .unwrap()
            .contains("scoped chart conventions")
    );
    let notebook = &requests[2]["messages"][4]["content"][0];
    assert_eq!(notebook["tool_use_id"], "notebook");
    assert!(
        notebook["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Cell plot")
    );
    assert_eq!(notebook["content"][1], image["content"][0]);
    let broken = &requests[3]["messages"][6]["content"][0];
    assert_eq!(broken["is_error"], true);
    assert!(
        broken["content"]
            .as_str()
            .unwrap()
            .contains("invalid image")
    );
    let artifact =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../output/claude-parity-files");
    std::fs::create_dir_all(&artifact).unwrap();
    std::fs::write(
        artifact.join("native-media-messages.json"),
        serde_json::to_vec_pretty(&*requests).unwrap(),
    )
    .unwrap();
    println!(
        "Observed actual Messages HTTP requests: exact image and notebook PNG bytes, scoped workspace guidance, malformed image is_error=true; trace output/claude-parity-files/native-media-messages.json"
    );
    server.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn sandbox_bash_subprocesses_see_their_own_session_identity_after_fork() {
    use nanocodex_agent::ForkRequest;
    use nanocodex_claude_tools::{
        SessionEnvironment,
        bash::{BashRequest, BashResult, ClaudeBash, SandboxBashExecutor},
    };
    /// A host sandbox that runs a real bash subprocess. Its configured
    /// environment carries spoofed identities, which the session must override.
    struct RealBash;
    impl SandboxBashExecutor for RealBash {
        async fn execute(&self, request: BashRequest) -> Result<BashResult, String> {
            let mut command = std::process::Command::new("bash");
            command
                .arg("-c")
                .arg(&request.command)
                .env(SessionEnvironment::SESSION_ID_VAR, "spoofed-session")
                .env(SessionEnvironment::ROOT_SESSION_ID_VAR, "spoofed-root");
            request.apply_session(&mut command);
            let output = command.output().map_err(|error| error.to_string())?;
            Ok(BashResult {
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                exit_code: output.status.code().unwrap_or(-1),
                truncated: false,
            })
        }
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    let command = r#"printf '%s|%s' "$CODEX_THREAD_ID" "$NANOCODEX_ROOT_SESSION_ID""#;
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = requests.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let log = log.clone();
            async move {
                let mut log = log.lock().unwrap();
                log.push(body);
                let (block, stop) = if log.len() % 2 == 1 {
                    let id = format!("b{}", log.len());
                    (
                        json!({"type":"tool_use","id":id,"name":"Bash","input":{"command":command}}),
                        "tool_use",
                    )
                } else {
                    (json!({"type":"text","text":"done"}), "end_turn")
                };
                ([("content-type", "text/event-stream")], sse(block, stop))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic",
    );
    let (parent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .sandbox_bash(Arc::new(ClaudeBash::new(RealBash)))
        .build()
        .unwrap();
    let stdout_of_last_tool_result = |request: &Value| {
        let messages = request["messages"].as_array().unwrap();
        let result = &messages.last().unwrap()["content"][0];
        assert_eq!(result["type"], "tool_result");
        serde_json::from_str::<Value>(result["content"].as_str().unwrap()).unwrap()["stdout"]
            .as_str()
            .unwrap()
            .to_owned()
    };

    parent.prompt("ids").await.unwrap().result().await.unwrap();
    let root_id = parent.session_id().to_owned();
    assert_eq!(
        stdout_of_last_tool_result(&requests.lock().unwrap()[1]),
        format!("{root_id}|{root_id}")
    );

    let (child, _) = parent.fork(ForkRequest::latest()).await.unwrap();
    let child_id = child.session_id().to_owned();
    assert_ne!(child_id, root_id);
    child
        .prompt("ids again")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(
        stdout_of_last_tool_result(&requests.lock().unwrap()[3]),
        format!("{child_id}|{root_id}")
    );
    server.abort();
}
