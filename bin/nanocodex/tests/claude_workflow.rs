//! Real CLI + QuickJS + Registry journey; only inference is synthetic.
use axum::{Json, Router, routing::post};
use serde_json::{Value, json};
use std::{
    path::Path,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpListener, process::Command};

fn sse(block: Value) -> String {
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
    [json!({"type":"message_start","message":{"id":"fixture","role":"assistant","model":"claude-sonnet-5-5","content":[],"usage":{"input_tokens":1,"output_tokens":0}}}),json!({"type":"content_block_start","index":0,"content_block":start}),json!({"type":"content_block_delta","index":0,"delta":delta}),json!({"type":"content_block_stop","index":0}),json!({"type":"message_delta","delta":{"stop_reason":if tool {"tool_use"} else {"end_turn"}},"usage":{"output_tokens":1}}),json!({"type":"message_stop"})].iter().map(|v|format!("data: {v}\n\n")).collect()
}
fn tool(stage: usize, name: &str, input: Value) -> Value {
    json!({"type":"tool_use","id":format!("wf-call-{stage}"),"name":name,"input":input})
}
fn result(body: &Value) -> Option<&Value> {
    body["messages"]
        .as_array()?
        .iter()
        .rev()
        .filter_map(|m| m["content"].as_array())
        .flatten()
        .find(|b| b["type"] == "tool_result")
}
fn parsed(body: &Value) -> Value {
    let value = &result(body).unwrap()["content"];
    let text = value.as_str().map(str::to_owned).unwrap_or_else(|| {
        value
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect()
    });
    serde_json::from_str(&text).unwrap_or(json!(text))
}
fn command(workspace: &Path, endpoint: &str, opt_in: bool) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_nanocodex"));
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
            "synthetic-workflow-key",
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
    if opt_in {
        cmd.arg("--claude-workflows");
    }
    cmd.arg("Run the synthetic workflow journey.");
    cmd
}
const SCRIPT: &str = r#"export const meta = {name:'fixture',description:'Registry workflow fixture',phases:[{title:'Review'},{title:'Verify'}]};
await phase('Review');
const values = await pipeline(args,
  item => agent('WORKFLOW_CHILD_' + item, {label:item, phase:'Review'}),
  value => parallel([() => agent('WORKFLOW_CHILD_verify_' + value, {phase:'Verify'})])
);
return {values, sandbox:[typeof process,typeof require,typeof fetch], catalog:ALL_TOOLS.map(t=>t.name)};
"#;
const SLOW: &str = "export const meta = {name:'slow',description:'Cancellable registry workflow'}; return await agent('WORKFLOW_CHILD_SLOW');";
#[tokio::test]
async fn native_workflow_registry_sandbox_resume_and_stop() {
    let artifact = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../output/claude-workflow")
        .join(uuid::Uuid::new_v4().to_string());
    let workspace = artifact.join("workspace");
    std::fs::create_dir_all(workspace.join("home")).unwrap();
    std::fs::create_dir_all(workspace.join(".claude/workflows")).unwrap();
    std::fs::write(workspace.join(".claude/workflows/fixture.js"), SCRIPT).unwrap();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let stage = Arc::new(Mutex::new(0usize));
    let ids = Arc::new(Mutex::new(Vec::<String>::new()));
    let paths = Arc::new(Mutex::new(Vec::<String>::new()));
    let log = requests.clone();
    let count = stage.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let app=Router::new().route("/v1/messages",post(move |Json(body):Json<Value>| {let log=log.clone();let count=count.clone();let ids=ids.clone();let paths=paths.clone();async move {
        let first=body["messages"][0]["content"].to_string();let child=first.contains("WORKFLOW_CHILD_");
        log.lock().unwrap().push(json!({"child":child,"request":body}));
        let reply=if child {
            assert!(!body["tools"].as_array().unwrap().iter().any(|t|t["name"]=="Workflow"),"Workflow leaked to child");
            if first.contains("WORKFLOW_CHILD_SLOW") {tokio::time::sleep(Duration::from_secs(30)).await;}
            if result(&body).is_none() {let value=if first.contains("verify_") {"verified"}else if first.contains("WORKFLOW_CHILD_a") {"reviewed-a"}else{"reviewed-b"};tool(100,"SubmitResult",json!({"output":value}))}else{json!({"type":"text","text":"child complete"})}
        } else {
            let stage={let mut n=count.lock().unwrap();let current=*n;*n+=1;current};
            if [2,4,6,10,13].contains(&stage) {let receipt=parsed(&body);ids.lock().unwrap().push(receipt["task_id"].as_str().unwrap_or_else(||panic!("missing workflow ID at stage {stage}: {receipt}")).into());if stage==2 {paths.lock().unwrap().push(receipt["scriptPath"].as_str().unwrap().into());}}
            let ids=ids.lock().unwrap().clone();let paths=paths.lock().unwrap().clone();
            match stage {
                0=>{assert!(body["tools"].as_array().unwrap().iter().any(|t|t["name"]=="Workflow"));tool(stage,"Workflow",json!({"script":"export const meta = {name: (() => 'bad')(), description:'No effects'}; await agent('SHOULD_NOT_SPAWN');"}))},
                1=>{assert_eq!(result(&body).unwrap()["is_error"],true);tool(stage,"Workflow",json!({"name":"fixture","args":["a","b"]}))},
                2=>tool(stage,"TaskOutput",json!({"task_id":ids[0],"block":true,"timeout":15000})),
                3=>{let receipt=parsed(&body);assert_eq!(receipt["status"],"completed","{receipt}");assert_eq!(receipt["output"]["values"],json!([["verified"],["verified"]]));assert_eq!(receipt["output"]["sandbox"],json!(["undefined","undefined","undefined"]));assert_eq!(receipt["output"]["catalog"],json!(["workflow_bridge"]));tool(stage,"Workflow",json!({"scriptPath":paths[0],"resumeFromRunId":ids[0],"args":["a","b"]}))},
                4=>tool(stage,"TaskOutput",json!({"task_id":ids[1],"block":true,"timeout":15000})),
                5=>{assert_eq!(parsed(&body)["status"],"completed");let starts=log.lock().unwrap().iter().filter(|r|r["child"]==true && result(&r["request"]).is_none()).count();assert_eq!(starts,4,"resume repeated completed effects");tool(stage,"Workflow",json!({"script":SLOW}))},
                6=>tool(stage,"TaskOutput",json!({"task_id":ids[2],"block":true,"timeout":100})),
                7=>{assert_eq!(parsed(&body)["status"],"running");tool(stage,"Workflow",json!({"script":SLOW,"resumeFromRunId":ids[2]}))},
                8=>{assert_eq!(result(&body).unwrap()["is_error"],true);tool(stage,"TaskStop",json!({"task_id":ids[2]}))},
                9=>{assert_eq!(parsed(&body)["status"],"stopped");tool(stage,"Workflow",json!({"script":SLOW,"resumeFromRunId":ids[2]}))},
                10=>tool(stage,"TaskOutput",json!({"task_id":ids[3],"block":true,"timeout":10000})),
                11=>{let receipt=parsed(&body);assert_eq!(receipt["status"],"failed","{receipt}");assert!(receipt["output"].to_string().contains("refusing to repeat"));tool(stage,"Workflow",json!({"scriptPath":"/etc/passwd"}))},
                12=>{assert_eq!(result(&body).unwrap()["is_error"],true);tool(stage,"Workflow",json!({"script":"export const meta = {name:'private',description:'No process tools'}; await tools.exec_command({cmd:'touch forbidden'});"}))},
                13=>tool(stage,"TaskOutput",json!({"task_id":ids[4],"block":true,"timeout":10000})),
                14=>{assert_eq!(parsed(&body)["status"],"failed");tool(stage,"TaskOutput",json!({"task_id":"wf_unknown","block":false}))},
                15=>{assert_eq!(result(&body).unwrap()["is_error"],true);json!({"type":"text","text":"workflow-journey-complete"})},
                _=>panic!("unexpected stage {stage}")
            }
        };([( "content-type","text/event-stream")],sse(reply))
    }}));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut cmd = command(&workspace, &endpoint, true);
    std::fs::write(artifact.join("scenario.txt"),format!("cargo +1.97.0 test -p nanocodex-bin --test claude_workflow -- --nocapture\nCLI: {cmd:?}\nExpected: private QuickJS helpers launch four actual Registry children; named/path scripts; completed resume produces zero new children; active resume rejected; stop interrupts child; uncertain resume refuses repeat; computed metadata, outside paths and process tools rejected.\n")).unwrap();
    let output = tokio::time::timeout(Duration::from_secs(70), cmd.output())
        .await
        .expect("workflow CLI deadline")
        .unwrap();
    std::fs::write(artifact.join("stdout.txt"), &output.stdout).unwrap();
    std::fs::write(artifact.join("stderr.txt"), &output.stderr).unwrap();
    std::fs::write(
        artifact.join("requests.json"),
        serde_json::to_vec_pretty(&*requests.lock().unwrap()).unwrap(),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}: {}",
        artifact.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("workflow-journey-complete"),
        "inspect {}",
        artifact.display()
    );
    assert!(!workspace.join("forbidden").exists());
    eprintln!("workflow evidence: {}", artifact.display());
    server.abort();
}
#[tokio::test]
async fn native_workflow_requires_explicit_cli_opt_in() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir(workspace.path().join("home")).unwrap();
    let observed = Arc::new(Mutex::new(false));
    let seen = observed.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let seen = seen.clone();
            async move {
                assert!(
                    !body["tools"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|t| t["name"] == "Workflow")
                );
                *seen.lock().unwrap() = true;
                (
                    [("content-type", "text/event-stream")],
                    sse(json!({"type":"text","text":"opt-in-required"})),
                )
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        command(workspace.path(), &endpoint, false).output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(*observed.lock().unwrap());
    server.abort();
}

#[tokio::test]
async fn native_workflow_pins_late_children_and_retained_children() {
    let artifact = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../output/claude-workflow")
        .join(format!("workspace-{}", uuid::Uuid::new_v4()));
    let workspace = artifact.join("workspace");
    std::fs::create_dir_all(workspace.join("home")).unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.name", "Workflow Fixture"],
        vec!["config", "user.email", "workflow@example.invalid"],
        vec!["commit", "--allow-empty", "-qm", "fixture"],
    ] {
        let out = Command::new("git")
            .current_dir(&workspace)
            .args(args)
            .output()
            .await
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = requests.clone();
    let stage = Arc::new(Mutex::new(0usize));
    let count = stage.clone();
    let ids = Arc::new(Mutex::new(Vec::<String>::new()));
    let trees = Arc::new(Mutex::new(Vec::<String>::new()));
    let retained_trees = trees.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let app=Router::new().route("/v1/messages",post(move |Json(body):Json<Value>| {let log=log.clone();let count=count.clone();let ids=ids.clone();let trees=trees.clone();async move {
        let first=body["messages"][0]["content"].to_string();let child=first.contains("WORKFLOW_PIN_");log.lock().unwrap().push(json!({"child":child,"request":body}));
        let reply=if child {
            let calls=body["messages"].as_array().unwrap().iter().filter_map(|m|m["content"].as_array()).flatten().filter(|b|b["type"]=="tool_result").count();
            if first.contains("WORKFLOW_PIN_DELAY") && calls==0 {tokio::time::sleep(Duration::from_millis(500)).await;}
            if first.contains("WORKFLOW_PIN_LATE") && calls==0 {tool(100,"Write",json!({"file_path":"late-child.txt","content":"late child remained in original worktree"}))}
            else if calls==0 || (first.contains("WORKFLOW_PIN_LATE") && calls==1) {tool(101,"SubmitResult",json!({"output":"pin-child-complete"}))}
            else {json!({"type":"text","text":"pin child finished"})}
        } else {
            let stage={let mut n=count.lock().unwrap();let current=*n;*n+=1;current};
            if [2,6].contains(&stage) {ids.lock().unwrap().push(parsed(&body)["task_id"].as_str().unwrap().into());}
            let ids=ids.lock().unwrap().clone();
            match stage {
                0=>tool(stage,"EnterWorktree",json!({"name":"workflow-pinned"})),
                1=>{trees.lock().unwrap().push(parsed(&body)["workspace"].as_str().unwrap().into());tool(stage,"Workflow",json!({"script":"export const meta={name:'retained',description:'Retained child pins'}; return await agent('WORKFLOW_PIN_RETAINED');"}))},
                2=>tool(stage,"TaskOutput",json!({"task_id":ids[0],"block":true,"timeout":10000})),
                3=>{assert_eq!(parsed(&body)["status"],"completed");tool(stage,"ExitWorktree",json!({"cleanup":true}))},
                4=>{assert_eq!(result(&body).unwrap()["is_error"],true);assert!(parsed(&body).to_string().contains("active background or child contexts"),"{}",parsed(&body));tool(stage,"Workflow",json!({"script":"export const meta={name:'late',description:'Delayed child pinned root'}; await agent('WORKFLOW_PIN_DELAY'); return await agent('WORKFLOW_PIN_LATE');"}))},
                5=>{// Capture the task ID from the subsequent TaskOutput receipt.
                    let id=parsed(&body)["task_id"].as_str().unwrap().to_owned();tool(stage,"TaskOutput",json!({"task_id":id,"block":false}))},
                6=>tool(stage,"ExitWorktree",json!({})),
                7=>{assert_eq!(parsed(&body)["status"],"exited");tool(stage,"TaskOutput",json!({"task_id":ids[1],"block":true,"timeout":10000}))},
                8=>{assert_eq!(parsed(&body)["status"],"completed","{}",parsed(&body));tool(stage,"Workflow",json!({"script":"export const meta={name:'moved',description:'No retargeted resume'}; return 1;","resumeFromRunId":ids[1]}))},
                9=>{assert_eq!(result(&body).unwrap()["is_error"],true);assert!(parsed(&body).to_string().contains("original workflow workspace"));json!({"type":"text","text":"workflow-workspace-complete"})},
                _=>panic!("unexpected stage {stage}")
            }
        };([( "content-type","text/event-stream")],sse(reply))
    }}));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut cmd = command(&workspace, &endpoint, true);
    std::fs::write(artifact.join("scenario.txt"),format!("cargo +1.97.0 test -p nanocodex-bin --test claude_workflow native_workflow_pins_late_children_and_retained_children -- --nocapture\nCLI: {cmd:?}\nExpected: a retained completed child prevents worktree cleanup after workflow settlement; parent ExitWorktree keeps worktree; later child Write still lands in original pinned worktree; resume cannot retarget.\n")).unwrap();
    let output = tokio::time::timeout(Duration::from_secs(40), cmd.output())
        .await
        .unwrap()
        .unwrap();
    std::fs::write(artifact.join("stdout.txt"), &output.stdout).unwrap();
    std::fs::write(artifact.join("stderr.txt"), &output.stderr).unwrap();
    std::fs::write(
        artifact.join("requests.json"),
        serde_json::to_vec_pretty(&*requests.lock().unwrap()).unwrap(),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}: {}",
        artifact.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("workflow-workspace-complete"),
        "inspect {}",
        artifact.display()
    );
    let tree = retained_trees.lock().unwrap()[0].clone();
    assert!(Path::new(&tree).join("late-child.txt").is_file());
    assert!(!workspace.join("late-child.txt").exists());
    eprintln!("workflow workspace evidence: {}", artifact.display());
    server.abort();
}

#[tokio::test]
async fn native_workflow_respects_read_denial_before_script_or_children() {
    let artifact = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../output/claude-workflow")
        .join(format!("denied-{}", uuid::Uuid::new_v4()));
    let workspace = artifact.join("workspace");
    std::fs::create_dir_all(workspace.join("home")).unwrap();
    let rules = workspace.join("permissions.json");
    std::fs::write(&rules, json!({"permissions":{"deny":["Read"]}}).to_string()).unwrap();
    std::fs::write(workspace.join("denied.js"),"export const meta={name:'denied',description:'Must not start'}; return await agent('READ_DENIED_CHILD');").unwrap();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = requests.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let log = log.clone();
            async move {
                let stage = {
                    let mut log = log.lock().unwrap();
                    let stage = log.len();
                    log.push(body.clone());
                    stage
                };
                let reply = match stage {
                    0 => tool(stage, "Workflow", json!({"scriptPath":"denied.js"})),
                    1 => {
                        assert_eq!(result(&body).unwrap()["is_error"], true);
                        assert!(parsed(&body).to_string().contains("permission denied"));
                        json!({"type":"text","text":"workflow-read-denied"})
                    }
                    _ => panic!("denied workflow spawned a child or retried"),
                };
                ([("content-type", "text/event-stream")], sse(reply))
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut cmd = command(&workspace, &endpoint, true);
    cmd.arg("--claude-permissions").arg(rules);
    std::fs::write(artifact.join("scenario.txt"),format!("cargo +1.97.0 test -p nanocodex-bin --test claude_workflow native_workflow_respects_read_denial_before_script_or_children -- --nocapture\nCLI: {cmd:?}\nExpected: Read deny blocks Workflow before reading/persisting script or spawning children.\n")).unwrap();
    let output = tokio::time::timeout(Duration::from_secs(30), cmd.output())
        .await
        .unwrap()
        .unwrap();
    std::fs::write(artifact.join("stdout.txt"), &output.stdout).unwrap();
    std::fs::write(artifact.join("stderr.txt"), &output.stderr).unwrap();
    std::fs::write(
        artifact.join("requests.json"),
        serde_json::to_vec_pretty(&*requests.lock().unwrap()).unwrap(),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(requests.lock().unwrap().len(), 2);
    assert!(!std::fs::read_dir(&workspace).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".nanocodex-workflow-")
    }));
    eprintln!("workflow denial evidence: {}", artifact.display());
    server.abort();
}

#[tokio::test]
async fn native_workflow_enforces_agent_concurrency_and_size_budgets() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let artifact = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../output/claude-workflow")
        .join(format!("budgets-{}", uuid::Uuid::new_v4()));
    let workspace = artifact.join("workspace");
    std::fs::create_dir_all(workspace.join("home")).unwrap();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = requests.clone();
    let stage = Arc::new(Mutex::new(0usize));
    let count = stage.clone();
    let ids = Arc::new(Mutex::new(Vec::<String>::new()));
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let starts = Arc::new(AtomicUsize::new(0));
    let first_wave = Arc::new(tokio::sync::Barrier::new(4));
    let peak_check = peak.clone();
    let starts_check = starts.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let app=Router::new().route("/v1/messages",post(move |Json(body):Json<Value>| {let log=log.clone();let count=count.clone();let ids=ids.clone();let active=active.clone();let peak=peak.clone();let starts=starts.clone();let first_wave=first_wave.clone();async move {
        let first=body["messages"][0]["content"].to_string();let child=first.contains("WORKFLOW_BUDGET_");log.lock().unwrap().push(json!({"child":child,"request":body}));
        let reply=if child {
            if result(&body).is_none() {let index=starts.fetch_add(1,Ordering::SeqCst);let n=active.fetch_add(1,Ordering::SeqCst)+1;peak.fetch_max(n,Ordering::SeqCst);if index<4 {tokio::time::timeout(Duration::from_secs(5),first_wave.wait()).await.expect("four actual workflow children should reach the provider together");}tokio::time::sleep(Duration::from_millis(50)).await;active.fetch_sub(1,Ordering::SeqCst);tool(100,"SubmitResult",json!({"output":"budget-child"}))}else{json!({"type":"text","text":"budget child finished"})}
        }else{
            let stage={let mut n=count.lock().unwrap();let current=*n;*n+=1;current};
            if [1,3,5,8].contains(&stage) {ids.lock().unwrap().push(parsed(&body)["task_id"].as_str().unwrap().into());}let ids=ids.lock().unwrap().clone();
            match stage {
                0=>tool(stage,"Workflow",json!({"script":"export const meta={name:'concurrent',description:'Bounded concurrency'}; return await parallel(Array.from({length:6},(_,i)=>()=>agent('WORKFLOW_BUDGET_parallel_'+i)));"})),
                1=>tool(stage,"TaskOutput",json!({"task_id":ids[0],"block":true,"timeout":15000})),
                2=>{assert_eq!(parsed(&body)["status"],"completed");assert_eq!(starts.load(Ordering::SeqCst),6);assert_eq!(peak.load(Ordering::SeqCst),4);tool(stage,"Workflow",json!({"script":"export const meta={name:'budget',description:'Agent admission limit'}; for(let i=0;i<16;i++) await agent('WORKFLOW_BUDGET_sequential_'+i);"}))},
                3=>tool(stage,"TaskOutput",json!({"task_id":ids[1],"block":true,"timeout":15000})),
                4=>{assert_eq!(parsed(&body)["status"],"failed");assert!(parsed(&body)["output"].to_string().contains("15 agent calls"));assert_eq!(starts.load(Ordering::SeqCst),21);tool(stage,"Workflow",json!({"script":"export const meta={name:'output',description:'Output bound'}; return 'x'.repeat(65537);"}))},
                5=>tool(stage,"TaskOutput",json!({"task_id":ids[2],"block":true,"timeout":10000})),
                6=>{assert_eq!(parsed(&body)["status"],"failed");assert!(parsed(&body)["output"].to_string().contains("64 KiB"));tool(stage,"Workflow",json!({"script":" ".repeat(524289)}))},
                7=>{assert_eq!(result(&body).unwrap()["is_error"],true);tool(stage,"Workflow",json!({"script":"export const meta={name:'spin',description:'Cancellable JavaScript loop'}; while(true) {}"}))},
                8=>tool(stage,"TaskStop",json!({"task_id":ids[3]})),
                9=>{assert_eq!(parsed(&body)["status"],"stopped");json!({"type":"text","text":"workflow-budgets-complete"})},
                _=>panic!("unexpected budget stage {stage}")
            }
        };([( "content-type","text/event-stream")],sse(reply))
    }}));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut cmd = command(&workspace, &endpoint, true);
    std::fs::write(artifact.join("scenario.txt"),format!("cargo +1.97.0 test -p nanocodex-bin --test claude_workflow native_workflow_enforces_agent_concurrency_and_size_budgets -- --nocapture\nCLI: {cmd:?}\nExpected: six real agents max4 concurrent; 16 sequential requests admit exactly15; >64KiB output and >512KiB script rejected; infinite JS loop stops through TaskStop.\n")).unwrap();
    let output = tokio::time::timeout(Duration::from_secs(45), cmd.output())
        .await
        .unwrap()
        .unwrap();
    std::fs::write(artifact.join("stdout.txt"), &output.stdout).unwrap();
    std::fs::write(artifact.join("stderr.txt"), &output.stderr).unwrap();
    std::fs::write(
        artifact.join("requests.json"),
        serde_json::to_vec_pretty(&*requests.lock().unwrap()).unwrap(),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}: {}",
        artifact.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("workflow-budgets-complete"),
        "inspect {}",
        artifact.display()
    );
    assert_eq!(peak_check.load(Ordering::SeqCst), 4);
    assert_eq!(starts_check.load(Ordering::SeqCst), 21);
    eprintln!("workflow budget evidence: {}", artifact.display());
    server.abort();
}

#[tokio::test]
async fn native_workflow_honors_agent_deny_and_ask_before_children() {
    let artifact = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../output/claude-workflow")
        .join(format!("agent-permissions-{}", uuid::Uuid::new_v4()));
    for (case, lane, rule, mode) in [
        ("deny", "deny", "Agent", "bypassPermissions"),
        (
            "deny-scoped",
            "deny",
            "Agent(prompt:WORKFLOW_PERMISSION_CHILD)",
            "bypassPermissions",
        ),
        ("ask", "ask", "Agent", "dontAsk"),
        (
            "ask-scoped",
            "ask",
            "Agent(prompt:WORKFLOW_PERMISSION_CHILD)",
            "dontAsk",
        ),
    ] {
        let case_artifact = artifact.join(case);
        let workspace = case_artifact.join("workspace");
        std::fs::create_dir_all(workspace.join("home")).unwrap();
        let rules = workspace.join("permissions.json");
        let mut permissions = json!({"allow":["Workflow"],"defaultMode":mode});
        permissions[lane] = json!([rule]);
        std::fs::write(&rules, json!({"permissions":permissions}).to_string()).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let log = requests.clone();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
        let app = Router::new().route(
            "/v1/messages",
            post(move |Json(body): Json<Value>| {
                let log = log.clone();
                async move {
                    let stage = {
                        let mut log = log.lock().unwrap();
                        let stage = log.len();
                        log.push(body.clone());
                        stage
                    };
                    let reply = match stage {
                        0 => tool(
                            stage,
                            "Workflow",
                            json!({"script":"export const meta={name:'restricted',description:'Agent admission required'}; return await agent('WORKFLOW_PERMISSION_CHILD');"}),
                        ),
                        1 => {
                            assert_eq!(result(&body).unwrap()["is_error"], true);
                            assert!(parsed(&body).to_string().contains("permission denied"));
                            json!({"type":"text","text":"workflow-agent-permission-denied"})
                        }
                        _ => panic!("restricted Workflow reached child provider or retried"),
                    };
                    ([("content-type", "text/event-stream")], sse(reply))
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut cmd = command(&workspace, &endpoint, true);
        cmd.arg("--claude-permissions").arg(&rules);
        std::fs::write(case_artifact.join("scenario.txt"), format!("cargo +1.97.0 test -p nanocodex-bin --test claude_workflow native_workflow_honors_agent_deny_and_ask_before_children -- --nocapture\nCLI: {cmd:?}\nInput: {lane} {rule}, mode {mode}, explicit allow Workflow.\nExpected: conservative aggregate Agent admission overrides Workflow allow; exactly two root requests, zero child requests and no persisted script.\n")).unwrap();
        let output = tokio::time::timeout(Duration::from_secs(30), cmd.output())
            .await
            .unwrap()
            .unwrap();
        std::fs::write(case_artifact.join("stdout.txt"), &output.stdout).unwrap();
        std::fs::write(case_artifact.join("stderr.txt"), &output.stderr).unwrap();
        std::fs::write(
            case_artifact.join("requests.json"),
            serde_json::to_vec_pretty(&*requests.lock().unwrap()).unwrap(),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "{}: {}",
            case_artifact.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("workflow-agent-permission-denied")
        );
        assert_eq!(requests.lock().unwrap().len(), 2);
        assert!(!std::fs::read_dir(&workspace).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".nanocodex-workflow-")
        }));
        server.abort();
    }
    eprintln!("workflow Agent admission evidence: {}", artifact.display());
}
