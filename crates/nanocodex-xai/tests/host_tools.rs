//! Public Responses HTTP journey: real files and authorized native processes,
//! caller-owned loopback MCP/browser proxies, native media and effect identity.
#![cfg(all(feature = "tools", unix))]
use axum::{Json, Router, routing::post};
use futures_util::StreamExt;
use nanocodex_agent::events::AgentEventKind;
use nanocodex_xai::{Xai, XaiClient};
use nanocodex_xai_tools::*;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("xai-tools-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn completion(output: Vec<Value>) -> String {
    format!(
        "data: {}\n\n",
        json!({"type":"response.completed","response":{"id":"fixture","status":"completed","output":output}})
    )
}
fn message(text: &str) -> Value {
    json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]})
}
#[derive(Clone)]
struct Proxy {
    http: reqwest::Client,
    base: String,
    calls: Arc<Mutex<Vec<HostRequest>>>,
}
impl Proxy {
    fn dispatch(&self, request: HostRequest, path: &str) -> HostFuture<Result<ToolOutput, String>> {
        self.calls.lock().unwrap().push(request.clone());
        let http = self.http.clone();
        let url = format!("{}{path}", self.base);
        Box::pin(async move {
            http.post(url).json(&json!({"tool":request.tool,"input":request.input,"call_id":request.context.call_id,"session_id":request.context.session_id,"turn_id":request.context.turn_id})).send().await.map_err(|e|e.to_string())?.error_for_status().map_err(|e|e.to_string())?.json::<ToolOutput>().await.map_err(|e|e.to_string())
        })
    }
}
impl XaiMcpProvider for Proxy {
    fn catalog(&self, context: HostContext) -> HostFuture<Result<Vec<McpToolDefinition>, String>> {
        let http = self.http.clone();
        let url = format!("{}/mcp/catalog", self.base);
        Box::pin(async move {
            http.post(url)
                .json(&json!({"session_id":context.session_id,"call_id":context.call_id}))
                .send()
                .await
                .map_err(|e| e.to_string())?
                .json()
                .await
                .map_err(|e| e.to_string())
        })
    }
    fn call(
        &self,
        request: HostRequest,
        _tool: McpToolDefinition,
    ) -> HostFuture<Result<ToolOutput, String>> {
        self.dispatch(request, "/mcp/call")
    }
}
impl ApprovedWebProvider for Proxy {
    fn capabilities(&self) -> Vec<WebCapability> {
        vec![WebCapability::Fetch]
    }
    fn call(&self, request: HostRequest) -> HostFuture<Result<ToolOutput, String>> {
        // Exact authorized destination; model input cannot widen this policy.
        if request.input["url"] != "https://fixture.example/approved" {
            return Box::pin(async { Err("URL not approved by host".into()) });
        }
        self.dispatch(request, "/web")
    }
}
#[tokio::test]
async fn native_files_shell_mcp_browser_and_media_over_responses_http() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let temp = Temp::new();
    let outside = Temp::new();
    std::fs::write(outside.0.join("secret.txt"), "outside").unwrap();
    std::os::unix::fs::symlink(&outside.0, temp.0.join("outside-link")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(temp.0.join("script.sh"), "#!/bin/sh\nprintf original").unwrap();
    std::fs::set_permissions(
        temp.0.join("script.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::fs::write(temp.0.join("private.txt"), "private before").unwrap();
    std::fs::set_permissions(
        temp.0.join("private.txt"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(temp.0.join("pipe"))
            .status()
            .unwrap()
            .success()
    );
    let steps = Arc::new(vec![
        (
            "write",
            json!({"file_path":"notes/note.txt","content":"violet\nsecond\n"}),
        ),
        (
            "read_file",
            json!({"target_file":"notes/note.txt","offset":1,"limit":1}),
        ),
        (
            "search_replace",
            json!({"file_path":"notes/note.txt","old_string":"violet","new_string":"indigo"}),
        ),
        ("glob", json!({"pattern":"**/*.txt"})),
        (
            "grep",
            json!({"pattern":"indigo","path":"notes","output_mode":"content"}),
        ),
        ("list_dir", json!({"target_directory":"."})),
        ("read_file", json!({"target_file":"../secret.txt"})),
        (
            "read_file",
            json!({"target_file":"outside-link/secret.txt"}),
        ),
        (
            "search_replace",
            json!({"file_path":"created.txt","old_string":"","new_string":"created"}),
        ),
        (
            "run_terminal_cmd",
            json!({"command":"printf 'shell-proof' > shell.txt; cat shell.txt","description":"write synthetic shell evidence","timeout":1000}),
        ),
        (
            "run_terminal_cmd",
            json!({"command":"touch forbidden.txt","description":"must be denied"}),
        ),
        (
            "run_terminal_cmd",
            json!({"command":"sleep 1; touch escaped-timeout.txt","description":"exercise bounded termination","timeout":30}),
        ),
        (
            "run_terminal_cmd",
            json!({"command":"head -c 100000 /dev/zero | tr '\\0' x","description":"exercise output capture cap","timeout":1000}),
        ),
        ("search_tool", json!({"query":"fixture echo"})),
        (
            "use_tool",
            json!({"tool_name":"fixture__echo","tool_input":{"value":"MCP proof"}}),
        ),
        (
            "use_tool",
            json!({"tool_name":"fixture__revoked","tool_input":{}}),
        ),
        (
            "web_fetch",
            json!({"url":"https://fixture.example/approved"}),
        ),
        ("web_fetch", json!({"url":"http://127.0.0.1/private"})),
        ("browser_snapshot", json!({"page":"approved-page"})),
        (
            "grep",
            json!({"pattern":".","output_mode":"files_with_matches","head_limit":1}),
        ),
        (
            "grep",
            json!({"pattern":"indigo","path":"notes/note.txt","head_limit":1}),
        ),
        (
            "search_replace",
            json!({"file_path":"script.sh","old_string":"original","new_string":"executable-after-edit"}),
        ),
        (
            "write",
            json!({"file_path":"private.txt","content":"private after"}),
        ),
        (
            "run_terminal_cmd",
            json!({"command":"./script.sh", "description":"Execute edited script with preserved mode", "timeout":1000}),
        ),
        ("read_file", json!({"target_file":"pipe"})),
    ]);
    let requests = Arc::new(Mutex::new(vec![]));
    let captured = requests.clone();
    let model_steps = steps.clone();
    let catalog_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let catalog_calls = catalog_count.clone();
    let proxy_receipts = Arc::new(Mutex::new(vec![]));
    let mcp_receipts = proxy_receipts.clone();
    let browser_receipts = proxy_receipts.clone();
    let app=Router::new()
        .route("/responses",post(move|Json(body):Json<Value>|{let captured=captured.clone();let steps=model_steps.clone();async move{
            let n={let mut log=captured.lock().unwrap();log.push(body);log.len()-1};
            let output=if let Some((tool,input))=steps.get(n){vec![json!({"type":"function_call","id":format!("item-{n}"),"call_id":format!("call-{n}"),"name":tool,"arguments":input.to_string()})]}else{vec![message("host journey complete")]};
            ([("content-type","text/event-stream")],completion(output))
        }}))
        .route("/mcp/catalog",post(move|Json(body):Json<Value>|{let calls=catalog_calls.clone();async move{
            assert!(!body["session_id"].as_str().unwrap().is_empty());
            let n=calls.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
            // An entry discovered on the first call is revoked before invocation.
            let mut tools=vec![McpToolDefinition{server:"fixture".into(),name:"echo".into(),description:"Echo synthetic fixture".into(),input_schema:json!({"type":"object","properties":{"value":{"type":"string"}}})}];
            if n==0{tools.push(McpToolDefinition{server:"fixture".into(),name:"revoked".into(),description:"fixture echo temporary".into(),input_schema:json!({"type":"object"})});}Json(tools)
        }}))
        .route("/mcp/call",post(move|Json(body):Json<Value>|{let receipts=mcp_receipts.clone();async move{
            receipts.lock().unwrap().push(body.clone());
            Json(ToolOutput::text("MCP proof").with_structured_result(body["input"].clone()).with_metadata(json!({"proxy":"mcp"})))
        }}))
        .route("/web",post(|Json(body):Json<Value>|async move{assert_eq!(body["input"]["url"],"https://fixture.example/approved");Json(ToolOutput::text("approved page"))}))
        .route("/browser",post(move|Json(body):Json<Value>|{let receipts=browser_receipts.clone();async move{
            receipts.lock().unwrap().push(body);
            let mut output=ToolOutput::text("browser screenshot").with_metadata(json!({"proxy":"browser"}));
            output.content.push(ToolContent::InputImage{image_url:"data:image/png;base64,iVBORw0KGgo=".into(),detail:Some("low".into())});Json(output)
        }}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let host_calls = Arc::new(Mutex::new(vec![]));
    let proxy = Arc::new(Proxy {
        http: reqwest::Client::new(),
        base: format!("http://{address}"),
        calls: host_calls.clone(),
    });
    let browser_proxy = proxy.clone();
    let browser=XaiHostTools::new(vec![ToolDefinition{name:"browser_snapshot".into(),description:"Read an approved browser page".into(),parameters:json!({"type":"object","properties":{"page":{"type":"string"}},"required":["page"]})}],move|request|{
        if request.input["page"]!="approved-page"{return Box::pin(async{Err("page not approved".into())});}
        browser_proxy.dispatch(request,"/browser")
    }).unwrap();
    let authorized = Arc::new(Mutex::new(vec![]));
    let shell_authorized = authorized.clone();
    let shell = AuthorizedShell::new(&temp.0, "/bin/sh", move |request| {
        shell_authorized
            .lock()
            .unwrap()
            .push(request.context.call_id.clone());
        if request.command.contains("forbidden") {
            return Err("exact command is not allowed".into());
        }
        Ok(())
    })
    .unwrap()
    .environment(BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]));
    let (agent, mut events) = Xai::new(
        XaiClient::new(
            reqwest::Client::new(),
            format!("http://{address}/responses"),
            "synthetic",
        ),
        "grok-4.6",
    )
    .workspace_files(Arc::new(XaiWorkspaceFiles::new(&temp.0).unwrap()))
    .bash(Arc::new(XaiBash::new(Arc::new(shell))))
    .mcp(Arc::new(XaiMcp::new(proxy.clone())))
    .approved_web(Arc::new(XaiWeb::new(proxy)))
    .host_tools(Arc::new(browser))
    .build()
    .unwrap();
    let result = agent
        .prompt("Exercise the installed authorized capabilities")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(result.final_message(), "host journey complete");
    assert_eq!(
        std::fs::read_to_string(temp.0.join("notes/note.txt")).unwrap(),
        "indigo\nsecond\n"
    );
    assert_eq!(
        std::fs::read_to_string(temp.0.join("shell.txt")).unwrap(),
        "shell-proof"
    );
    assert!(!temp.0.join("forbidden.txt").exists());
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(!temp.0.join("escaped-timeout.txt").exists());
    assert_eq!(catalog_count.load(std::sync::atomic::Ordering::SeqCst), 3);
    assert_eq!(authorized.lock().unwrap().len(), 5);
    assert_eq!(
        std::fs::metadata(temp.0.join("script.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    assert_eq!(
        std::fs::metadata(temp.0.join("private.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::read_to_string(temp.0.join("private.txt")).unwrap(),
        "private after"
    );
    agent.shutdown().await.unwrap();
    let mut results = vec![];
    while let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(20), events.next()).await
    {
        event
            .data()
            .unwrap_or_else(|error| panic!("event {:?}: {error}", event.kind));
        if event.kind == AgentEventKind::ToolResult {
            results.push(event.decode_payload::<Value>().unwrap());
        }
    }
    assert_eq!(results.len(), steps.len());
    for index in [6, 7, 10, 11, 15, 17, 24] {
        assert_eq!(results[index]["status"], "failed", "{}", results[index]);
    }
    assert_eq!(
        results[12]["structured_result"]["stdout"]
            .as_str()
            .unwrap()
            .len(),
        65536
    );
    assert_eq!(results[12]["metadata"]["truncated"], true);
    assert_eq!(results[14]["structured_result"]["value"], "MCP proof");
    assert_eq!(results[18]["metadata"]["proxy"], "browser");
    assert_eq!(results[19]["metadata"]["truncated"], true);
    assert_eq!(results[20]["metadata"]["truncated"], false);
    assert_eq!(
        results[23]["structured_result"]["stdout"],
        "executable-after-edit"
    );
    assert_eq!(results[23]["structured_result"]["exit_code"], 0);
    let requests = requests.lock().unwrap();
    let final_input = requests.last().unwrap()["input"].as_array().unwrap();
    let screenshot = final_input
        .iter()
        .find(|i| i["call_id"] == "call-18" && i["type"] == "function_call_output")
        .unwrap();
    assert_eq!(screenshot["output"][1]["type"], "input_image");
    assert_eq!(requests[0]["tools"].as_array().unwrap().len(), 11);
    for request in host_calls.lock().unwrap().iter() {
        assert!(!request.context.session_id.is_empty());
        assert!(!request.context.turn_id.is_empty());
        assert!(request.context.call_id.starts_with("call-"));
    }
    let trace = json!({"requests":*requests,"results":results,"proxy_receipts":*proxy_receipts.lock().unwrap(),"filesystem":{"note":"indigo\nsecond\n","shell":"shell-proof","denied_absent":true,"timeout_child_absent":true,"script_mode":"0755","private_mode":"0600","edited_script_stdout":"executable-after-edit"}});
    std::fs::create_dir_all("../../output/xai").unwrap();
    std::fs::write(
        "../../output/xai/host-tools-journey.json",
        serde_json::to_vec_pretty(&trace).unwrap(),
    )
    .unwrap();
    server.abort();
}

/// A caller-owned task runtime. Runs a real nested xAI lifecycle and checkpoints
/// only its own task registry. The adapter itself never fabricates child output.
struct ChildTasks {
    endpoint: String,
    state: Arc<Mutex<BTreeMap<String, Value>>>,
}
impl XaiTaskProvider for ChildTasks {
    fn capabilities(&self) -> Vec<TaskCapability> {
        vec![TaskCapability::Task, TaskCapability::GetTaskOutput]
    }
    fn call(&self, request: HostRequest) -> HostFuture<Result<ToolOutput, String>> {
        let endpoint = self.endpoint.clone();
        let state = self.state.clone();
        Box::pin(async move {
            match request.tool.as_str() {
                "task" => {
                    let (child, _) = Xai::new(
                        XaiClient::new(reqwest::Client::new(), endpoint, "fixture"),
                        request.context.model,
                    )
                    .build()
                    .map_err(|e| e.to_string())?;
                    let result = child
                        .prompt(request.input["prompt"].as_str().ok_or("missing prompt")?)
                        .await
                        .map_err(|e| e.to_string())?
                        .result()
                        .await
                        .map_err(|e| e.to_string())?;
                    child.shutdown().await.map_err(|e| e.to_string())?;
                    let id = format!("child-{}", request.context.call_id);
                    let result = json!({"subagent_id":id,"status":"completed","output":result.final_message(),"owner":request.context.session_id});
                    state.lock().unwrap().insert(id, result.clone());
                    Ok(ToolOutput::text(result.to_string()).with_structured_result(result))
                }
                "get_task_output" => {
                    let id = request.input["task_ids"][0]
                        .as_str()
                        .ok_or("missing task id")?;
                    let result = state
                        .lock()
                        .unwrap()
                        .get(id)
                        .cloned()
                        .ok_or("task not found")?;
                    if result["owner"] != request.context.session_id {
                        return Err("task is not owned by session".into());
                    }
                    Ok(ToolOutput::text(result.to_string()).with_structured_result(result))
                }
                _ => Err("capability not installed".into()),
            }
        })
    }
    fn checkpoint(&self, session_id: String) -> HostFuture<Result<Value, String>> {
        let state = self.state.clone();
        Box::pin(async move {
            let entries: BTreeMap<_, _> = state
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, v)| v["owner"] == session_id)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            Ok(json!(entries))
        })
    }
    fn restore(&self, session_id: String, checkpoint: Value) -> HostFuture<Result<(), String>> {
        let state = self.state.clone();
        Box::pin(async move {
            let entries: BTreeMap<String, Value> =
                serde_json::from_value(checkpoint).map_err(|e| e.to_string())?;
            if entries
                .values()
                .any(|v| v["owner"] != session_id || v["status"] != "completed")
            {
                return Err("checkpoint contains another owner or unreconciled task".into());
            }
            state.lock().unwrap().extend(entries);
            Ok(())
        })
    }
}
#[tokio::test]
async fn task_callbacks_run_real_child_and_restore_owned_checkpoint() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(vec![]));
    let captured = requests.clone();
    let app=Router::new().route("/parent",post(move|Json(body):Json<Value>|{let captured=captured.clone();async move{
        let n={let mut requests=captured.lock().unwrap();requests.push(body);requests.len()};
        let output=match n{
            1=>vec![json!({"type":"function_call","call_id":"task-start","name":"task","arguments":json!({"prompt":"Return a synthetic child result","description":"Nested fixture","run_in_background":false}).to_string()})],
            3=>vec![json!({"type":"function_call","call_id":"task-read","name":"get_task_output","arguments":json!({"task_ids":["child-task-start"]}).to_string()})],
            _=>vec![message("child result recorded")]
        };([( "content-type","text/event-stream")],completion(output))
    }})).route("/child",post(|Json(body):Json<Value>|async move{
        assert!(body["input"].as_array().unwrap().iter().any(|m|m["content"]=="Return a synthetic child result"));
        ([("content-type","text/event-stream")],completion(vec![message("verified child result")]))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let state = Arc::new(Mutex::new(BTreeMap::new()));
    let tasks = Arc::new(XaiTasks::new(Arc::new(ChildTasks {
        endpoint: format!("http://{address}/child"),
        state: state.clone(),
    })));
    let (agent, _) = Xai::new(
        XaiClient::new(
            reqwest::Client::new(),
            format!("http://{address}/parent"),
            "fixture",
        ),
        "grok-4.6",
    )
    .tasks(tasks.clone())
    .build()
    .unwrap();
    agent
        .prompt("delegate fixture")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let session = state.lock().unwrap()["child-task-start"]["owner"]
        .as_str()
        .unwrap()
        .to_owned();
    let checkpoint = tasks.checkpoint(session.clone()).await.unwrap();
    assert_eq!(
        checkpoint["child-task-start"]["output"],
        "verified child result"
    );
    let disk = Temp::new();
    std::fs::write(
        disk.0.join("tasks.json"),
        serde_json::to_vec(&checkpoint).unwrap(),
    )
    .unwrap();
    state.lock().unwrap().clear();
    let restored: Value =
        serde_json::from_slice(&std::fs::read(disk.0.join("tasks.json")).unwrap()).unwrap();
    assert!(
        tasks
            .restore("different-session".into(), restored.clone())
            .await
            .is_err()
    );
    tasks.restore(session, restored).await.unwrap();
    agent
        .prompt("retrieve prior child")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    agent.shutdown().await.unwrap();
    let requests = requests.lock().unwrap();
    let result = requests.last().unwrap()["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["type"] == "function_call_output" && i["call_id"] == "task-read")
        .unwrap();
    assert!(
        result["output"]
            .as_str()
            .unwrap()
            .contains("verified child result")
    );
    std::fs::create_dir_all("../../output/xai").unwrap();
    std::fs::write(
        "../../output/xai/tasks-checkpoint-journey.json",
        serde_json::to_vec_pretty(
            &json!({"requests":*requests,"checkpoint":checkpoint,"wrong_owner_rejected":true}),
        )
        .unwrap(),
    )
    .unwrap();
    server.abort();
}
