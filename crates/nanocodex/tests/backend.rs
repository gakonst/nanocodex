//! Public facade journeys: only the external model is replaced by loopback SSE.
#![cfg(all(feature = "native", feature = "openai", not(target_family = "wasm")))]
#![allow(missing_docs)]
use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use nanocodex::{Backend, HarnessFamily, Nanocodex};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn evidence(name: &str, value: &Value) {
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../output/backend-facade");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join(name),
        serde_json::to_vec_pretty(value).unwrap(),
    )
    .unwrap();
}

fn backend(family: HarnessFamily) -> Backend {
    match family {
        HarnessFamily::Codex => Backend::codex("synthetic-key").unwrap(),
        #[cfg(feature = "claude")]
        HarnessFamily::Claude => Backend::claude("synthetic-key").unwrap(),
        #[allow(unreachable_patterns)]
        _ => panic!("disabled fixture family"),
    }
}

fn families() -> Vec<HarnessFamily> {
    vec![
        HarnessFamily::Codex,
        #[cfg(feature = "claude")]
        HarnessFamily::Claude,
    ]
}

fn frame(family: HarnessFamily, body: &Value, n: usize, call: Option<(&str, Value)>) -> String {
    match family {
        HarnessFamily::Codex => {
            let output = if let Some((name, input)) = call {
                json!([{"type":"custom_tool_call","call_id":format!("call-{n}"),"name":name,"input":input["code"]}])
            } else {
                json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":"workspace journey complete"}]}])
            };
            let response = json!({"type":"response.completed","response":{"id":format!("response-{n}"),"status":"completed","output":output}});
            format!("data: {response}\n\ndata: [DONE]\n\n")
        }
        HarnessFamily::Claude => {
            let block = match call.as_ref() {
                Some((name, _)) => {
                    json!({"type":"tool_use","id":format!("call-{n}"),"name":name,"input":{}})
                }
                None => json!({"type":"text","text":"workspace journey complete"}),
            };
            let mut frames = vec![
                json!({"type":"message_start","message":{"id":format!("message-{n}"),"role":"assistant","model":body["model"],"content":[],"usage":{"input_tokens":10,"output_tokens":0}}}),
                json!({"type":"content_block_start","index":0,"content_block":block}),
            ];
            if let Some((_, input)) = call.as_ref() {
                frames.push(json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":input.to_string()}}));
            }
            frames.extend([
                json!({"type":"content_block_stop","index":0}),
                json!({"type":"message_delta","delta":{"stop_reason":if call.is_some() {"tool_use"} else {"end_turn"}},"usage":{"output_tokens":3}}),
                json!({"type":"message_stop"}),
            ]);
            frames
                .into_iter()
                .map(|f| format!("data: {f}\n\n"))
                .collect()
        }
    }
}

#[tokio::test]
async fn selected_backend_runs_native_workspace_tools_and_recovers_from_tool_errors() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    for family in families() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(
            workspace.path().join("book.ipynb"),
            json!({"nbformat":4,"nbformat_minor":5,"metadata":{},"cells":[]}).to_string(),
        )
        .unwrap();
        let script = match family {
            HarnessFamily::Codex => vec![
                (
                    "exec",
                    json!({"code":"text(await tools.exec_command({cmd: \"printf 'native workspace' > note.txt; cat note.txt\"}));"}),
                ),
                (
                    "exec",
                    json!({"code":"text(await tools.exec_command({cmd: \"printf 'shell failed'; exit 9\"}));"}),
                ),
                (
                    "exec",
                    json!({"code":"text(await tools.exec_command({cmd: \"cat note.txt; printf 'recovered' > recovered.txt\"}));"}),
                ),
            ],
            HarnessFamily::Claude => vec![
                (
                    "Write",
                    json!({"file_path":"note.txt","content":"native workspace"}),
                ),
                ("Read", json!({"file_path":"note.txt"})),
                (
                    "Edit",
                    json!({"file_path":"note.txt","old_string":"native","new_string":"native edited"}),
                ),
                ("Glob", json!({"pattern":"*.txt"})),
                ("Grep", json!({"pattern":"native","path":"note.txt"})),
                (
                    "TaskCreate",
                    json!({"subject":"verify workspace","description":"Read and edit the fixture"}),
                ),
                ("TaskList", json!({})),
                (
                    "NotebookEdit",
                    json!({"notebook_path":"book.ipynb","new_source":"print('native')","cell_type":"code","edit_mode":"insert"}),
                ),
                (
                    "Bash",
                    json!({"command":"cat note.txt; printf 'recovered' > recovered.txt"}),
                ),
                ("Bash", json!({"command":"printf 'shell failed'; exit 9"})),
                ("Read", json!({"file_path":"missing.txt"})),
                ("Read", json!({"file_path":"recovered.txt"})),
            ],
        };
        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let observed = Arc::clone(&requests);
        let count = script.len();
        let app = Router::new().route(
            "/{*path}",
            post(move |headers: HeaderMap, Json(body): Json<Value>| {
                let observed = Arc::clone(&observed);
                let script = script.clone();
                async move {
                    if family == HarnessFamily::Claude {
                        assert_eq!(headers["x-api-key"], "synthetic-key");
                    } else {
                        assert_eq!(headers["authorization"], "Bearer synthetic-key");
                    }
                    let mut requests = observed.lock().unwrap();
                    let n = requests.len();
                    assert!(n <= script.len(), "unexpected request: {body}");
                    let response = frame(
                        family,
                        &body,
                        n,
                        script.get(n).map(|(name, input)| (*name, input.clone())),
                    );
                    requests.push(body);
                    ([("content-type", "text/event-stream")], response)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let endpoint = match family {
            HarnessFamily::Codex => format!("http://{address}/v1"),
            HarnessFamily::Claude => format!("http://{address}/v1/messages"),
        };
        let choice = backend(family).endpoint(endpoint).unwrap();
        let (agent, mut events) = Nanocodex::builder(choice)
            .workspace(workspace.path())
            .build()
            .unwrap();
        assert_eq!(agent.harness_family(), family);
        let result = tokio::time::timeout(Duration::from_secs(30), async {
            agent
                .prompt("Complete the workspace fixture journey")
                .await
                .unwrap()
                .await
                .unwrap()
        })
        .await
        .unwrap();
        assert_eq!(result.final_message(), "workspace journey complete");
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("recovered.txt")).unwrap(),
            "recovered"
        );
        let note = std::fs::read_to_string(workspace.path().join("note.txt")).unwrap();
        assert!(note.starts_with("native"));
        if family == HarnessFamily::Claude {
            assert_eq!(note, "native edited workspace");
            let notebook: Value = serde_json::from_str(
                &std::fs::read_to_string(workspace.path().join("book.ipynb")).unwrap(),
            )
            .unwrap();
            assert_eq!(notebook["cells"].as_array().unwrap().len(), 1);
        }
        agent.shutdown().await.unwrap();
        assert!(agent.prompt("after shutdown").await.is_err());
        drop(agent);
        let event = events.recv().await.expect("observable lifecycle events");
        eprintln!("family={family} first-event={event:?}");
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), count + 1);
        evidence(
            &format!("{family}-workspace-requests.json"),
            &json!(*requests),
        );
        let transcript = serde_json::to_string(&*requests).unwrap();
        assert!(
            transcript.contains("native workspace")
                || transcript.contains("native edited workspace")
        );
        assert!(transcript.contains("shell failed"));
        if family == HarnessFamily::Claude {
            assert!(
                transcript.contains("\"is_error\":true"),
                "failed Read must return a tool error"
            );
            assert!(transcript.contains("verify workspace"));
            for name in [
                "Bash",
                "Read",
                "Write",
                "Edit",
                "Glob",
                "Grep",
                "TaskCreate",
                "TaskGet",
                "TaskList",
                "TaskUpdate",
                "TodoWrite",
                "NotebookEdit",
                "spawn_agent",
                "wait_agent",
                "close_agent",
            ] {
                assert!(
                    requests[0]["tools"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|tool| tool["name"] == name),
                    "missing {name}"
                );
            }
        }
        eprintln!(
            "family={family} requests={} note={note:?} recovery=recovered shell-exit=9 native-tool-results-returned=true",
            requests.len()
        );
        server.abort();
    }
}

#[tokio::test]
async fn facade_reports_configuration_and_provider_errors() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    assert!(Backend::codex(" ").is_err());
    let secret = "synthetic-secret\ninvalid";
    let error = Backend::codex(secret).err().unwrap().to_string();
    assert!(!error.contains(secret));
    assert!(
        Backend::codex("synthetic")
            .unwrap()
            .endpoint("file:///private")
            .is_err()
    );
    assert!(
        Backend::codex("synthetic")
            .unwrap()
            .model(HarnessFamily::Claude.default_model())
            .is_err()
    );
    for family in families() {
        let temp = tempfile::tempdir().unwrap();
        assert!(
            Nanocodex::builder(backend(family))
                .workspace(temp.path().join("missing"))
                .build()
                .is_err()
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener,Router::new().route("/{*path}",post(|| async { (StatusCode::UNAUTHORIZED, Json(json!({"error":{"type":"authentication_error","message":"synthetic rejected credential"}}))) }))).await.unwrap();
        });
        let (agent, _) = Nanocodex::builder(
            backend(family)
                .endpoint(format!("http://{address}/v1"))
                .unwrap(),
        )
        .workspace(temp.path())
        .build()
        .unwrap();
        let error = tokio::time::timeout(Duration::from_secs(10), async {
            agent.prompt("authentication failure").await.unwrap().await
        })
        .await
        .unwrap()
        .err()
        .expect("provider error");
        eprintln!("family={family} provider-error={error}");
        assert!(!error.to_string().contains("synthetic-key"));
        agent.shutdown().await.unwrap();
        server.abort();
    }
}

#[test]
fn missing_tokio_runtime_is_an_error() {
    for family in families() {
        let error = Nanocodex::builder(backend(family))
            .build()
            .err()
            .expect("runtime required");
        assert!(error.to_string().contains("Tokio"));
    }
}

#[cfg(feature = "durability")]
#[tokio::test]
async fn common_builder_replays_durable_tool_turn_without_repeating_shell_effect() {
    use nanocodex::{
        DurableAgentExt, PromptRequest,
        durability::{DurableSession, MemoryStore},
    };
    let _ = rustls::crypto::ring::default_provider().install_default();
    for family in families() {
        let temp = tempfile::tempdir().unwrap();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = Arc::clone(&calls);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route("/{*path}", post(move |Json(body): Json<Value>| {
            let n = count.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
            async move {
                let call = if n == 0 {
                    Some(match family {
                        HarnessFamily::Codex => ("exec",json!({"code":"text(await tools.exec_command({cmd: \"printf x >> receipt.txt\"}));"})),
                        HarnessFamily::Claude => ("Bash",json!({"command":"printf x >> receipt.txt"})),
                    })
                } else { None };
                ([("content-type","text/event-stream")],frame(family,&body,n,call))
            }
        }));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let store = MemoryStore::new().unwrap();
        for _ in 0..2 {
            let state = DurableSession::open(store.clone(), "facade-native-durable")
                .await
                .unwrap();
            let (agent, _) = Nanocodex::builder(
                backend(family)
                    .endpoint(format!("http://{address}/v1"))
                    .unwrap(),
            )
            .workspace(temp.path())
            .durability(state)
            .await
            .unwrap()
            .build()
            .unwrap();
            let result = tokio::time::timeout(Duration::from_secs(30), async {
                agent
                    .prompt(PromptRequest::new("Record one receipt").request_id("one-receipt"))
                    .await
                    .unwrap()
                    .await
                    .unwrap()
            })
            .await
            .unwrap();
            assert_eq!(result.final_message(), "workspace journey complete");
            agent.shutdown().await.unwrap();
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(
            std::fs::read_to_string(temp.path().join("receipt.txt")).unwrap(),
            "x"
        );
        eprintln!("family={family} durable-reopen=model-requests:2 shell-receipt:x (not xx)");
        server.abort();
    }
}

#[tokio::test]
async fn native_subagents_inherit_workspace_tools_and_return_structured_results() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    for family in families() {
        let temp = tempfile::tempdir().unwrap();
        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let received = Arc::clone(&requests);
        let root_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let child_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let child_observed = Arc::clone(&child_count);
        let app = Router::new().route("/{*path}",post(move |Json(body):Json<Value>| {
            let received = Arc::clone(&received);
            let root_count = Arc::clone(&root_count);
            let child_count = Arc::clone(&child_count);
            async move {
                let messages = body.get("messages").or_else(||body.get("input")).unwrap().as_array().unwrap();
                let child = messages.iter().filter(|m|m["role"]=="user")
                    .filter_map(|m|m["content"].as_array()).flatten()
                    .filter(|b|b["type"]=="text" || b["type"]=="input_text")
                    .any(|b|b["text"].as_str().is_some_and(|s|s.contains("facade-child-task")));
                let n = if child { &child_count } else { &root_count }.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
                let task = json!({"role":"fixture worker","task":"facade-child-task","output_contract":{"kind":"string"}});
                let call = match (family,child,n) {
                    (HarnessFamily::Codex,false,0) => Some(("exec",json!({"code":format!("const child = await tools.spawn_agent({task}); text(child); text(await tools.wait_agent({{agent_ids:[child.agent_id],timeout_ms:10000}})); text(await tools.close_agent({{agent_id:child.agent_id}}));")}))),
                    (HarnessFamily::Codex,true,0) => Some(("exec",json!({"code":"text(await tools.exec_command({cmd: \"printf 'child native tools' > child.txt\"})); text(await tools.submit_result({output:'child receipt'}));"}))),
                    (HarnessFamily::Claude,false,0) => Some(("spawn_agent",task)),
                    (HarnessFamily::Claude,false,1) => Some(("wait_agent",json!({"agent_ids":[1],"timeout_ms":10000}))),
                    (HarnessFamily::Claude,false,2) => Some(("close_agent",json!({"agent_id":1}))),
                    (HarnessFamily::Claude,true,0) => Some(("Bash",json!({"command":"printf 'child native tools' > child.txt"}))),
                    (HarnessFamily::Claude,true,1) => Some(("submit_result",json!({"output":"child receipt"}))),
                    _ => None,
                };
                let response = frame(family,&body,if child { n+100 } else { n },call);
                received.lock().unwrap().push(body);
                ([("content-type","text/event-stream")],response)
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (agent, _) = Nanocodex::builder(
            backend(family)
                .endpoint(format!("http://{address}/v1"))
                .unwrap(),
        )
        .workspace(temp.path())
        .build()
        .unwrap();
        let result = tokio::time::timeout(Duration::from_secs(30), async {
            agent
                .prompt("Delegate the fixture worker and collect its result")
                .await
                .unwrap()
                .await
                .unwrap()
        })
        .await
        .unwrap();
        assert_eq!(result.final_message(), "workspace journey complete");
        assert!(
            child_observed.load(std::sync::atomic::Ordering::SeqCst) >= 2,
            "child model transport was exercised"
        );
        assert_eq!(
            std::fs::read_to_string(temp.path().join("child.txt")).unwrap(),
            "child native tools"
        );
        evidence(
            &format!("{family}-subagent-requests.json"),
            &json!(*requests.lock().unwrap()),
        );
        let transcript = serde_json::to_string(&*requests.lock().unwrap()).unwrap();
        assert!(transcript.contains("child receipt"));
        assert!(
            transcript.contains("completed"),
            "root receives completed structured child result"
        );
        agent.shutdown().await.unwrap();
        eprintln!(
            "family={family} child-native-filesystem=child.txt child-receipt=accepted root-wait=completed close=completed"
        );
        server.abort();
    }
}

#[cfg(unix)]
#[tokio::test]
async fn shutdown_joins_active_root_and_child_shell_processes() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    for family in families() {
        for implicit in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let child_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let app = Router::new().route("/{*path}",post(move |Json(body):Json<Value>| {
            let root_count = Arc::clone(&root_count);
            let child_count = Arc::clone(&child_count);
            async move {
                let messages = body.get("messages").or_else(||body.get("input")).unwrap().as_array().unwrap();
                let child = messages.iter().filter(|m|m["role"]=="user")
                    .filter_map(|m|m["content"].as_array()).flatten()
                    .filter(|b|b["type"]=="text" || b["type"]=="input_text")
                    .any(|b|b["text"].as_str().is_some_and(|s|s.contains("facade-child-task")));
                let n = if child { &child_count } else { &root_count }.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
                let spawn = json!({"role":"fixture worker","task":"facade-child-task","output_contract":{"kind":"string"}});
                let root_shell = "echo $$ > root.pid; while :; do sleep 1; done";
                let child_shell = "echo $$ > child.pid; while :; do sleep 1; done";
                let call = match (family,child,n) {
                    (HarnessFamily::Codex,false,0) => Some(("exec",json!({"code":format!("text(await tools.spawn_agent({spawn})); text(await tools.exec_command({{cmd:{},yield_time_ms:1000}}));",json!(root_shell))}))),
                    (HarnessFamily::Codex,true,0) => Some(("exec",json!({"code":format!("text(await tools.exec_command({{cmd:{},yield_time_ms:1000}}));",json!(child_shell))}))),
                    (HarnessFamily::Claude,false,0) => Some(("spawn_agent",spawn)),
                    (HarnessFamily::Claude,false,1) => Some(("Bash",json!({"command":root_shell}))),
                    (HarnessFamily::Claude,true,0) => Some(("Bash",json!({"command":child_shell}))),
                    _ => None,
                };
                ([("content-type","text/event-stream")],frame(family,&body,if child { n+100 } else { n },call))
            }
        }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let (agent, _) = Nanocodex::builder(
                backend(family)
                    .endpoint(format!("http://{address}/v1"))
                    .unwrap(),
            )
            .workspace(temp.path())
            .build()
            .unwrap();
            let _turn = agent
                .prompt("Start the fixture worker and retain shell processes")
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(20), async {
                while ["root.pid", "child.pid"].iter().any(|name| {
                    std::fs::read_to_string(temp.path().join(name))
                        .map_or(true, |pid| pid.trim().parse::<u32>().is_err())
                }) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("root and child must both execute real shell processes");
            let pids: Vec<String> = ["root.pid", "child.pid"]
                .iter()
                .map(|p| {
                    std::fs::read_to_string(temp.path().join(p))
                        .unwrap()
                        .trim()
                        .to_owned()
                })
                .collect();
            for pid in &pids {
                assert!(
                    std::process::Command::new("kill")
                        .args(["-0", pid])
                        .status()
                        .unwrap()
                        .success(),
                    "fixture process was alive"
                );
            }
            let callbacks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let observed = Arc::clone(&callbacks);
            let agent = agent
                .with_shutdown_hook(move || async move {
                    observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                })
                .unwrap();
            let clone = agent.clone();
            if implicit {
                drop(agent);
                assert_eq!(callbacks.load(std::sync::atomic::Ordering::SeqCst), 0);
                drop(clone);
                tokio::time::timeout(Duration::from_secs(20), async {
                    while callbacks.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("last handle drop must run cleanup");
            } else {
                tokio::time::timeout(Duration::from_secs(20), async {
                    let (first, second) = tokio::join!(agent.shutdown(), clone.shutdown());
                    first.unwrap();
                    second.unwrap();
                })
                .await
                .unwrap();
                agent.shutdown().await.unwrap();
                assert!(agent.prompt("after joined cleanup").await.is_err());
            }
            assert_eq!(callbacks.load(std::sync::atomic::Ordering::SeqCst), 1);
            // Deliberately no grace period: shutdown is the completion receipt.
            for pid in &pids {
                assert!(
                    !std::process::Command::new("kill")
                        .args(["-0", pid])
                        .output()
                        .unwrap()
                        .status
                        .success(),
                    "process {pid} survived shutdown"
                );
            }
            evidence(
                &format!("{family}-process-cleanup-drop-{implicit}.json"),
                &json!({"family":family.to_string(),"implicit_drop":implicit,"pids":pids,
                    "alive_before":true,"alive_after":false,"cleanup_callbacks":1}),
            );
            eprintln!(
                "family={family} implicit-drop={implicit} cleanup-receipt=root-and-child-PIDs-reaped hook-count=1 concurrent-clones=joined pids={pids:?}"
            );
            server.abort();
        }
    }
}

#[tokio::test]
async fn shutdown_cleanup_survives_a_cancelled_waiter() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    for family in families() {
        let workspace = tempfile::tempdir().unwrap();
        let (agent, _) = Nanocodex::builder(backend(family))
            .workspace(workspace.path())
            .build()
            .unwrap();
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, gate) = tokio::sync::oneshot::channel();
        let receipt = workspace.path().join("shutdown-receipt");
        let written = receipt.clone();
        let agent = agent
            .with_shutdown_hook(move || async move {
                let _ = entered.send(());
                gate.await.unwrap();
                std::fs::write(written, "cleanup completed once").unwrap();
                Ok(())
            })
            .unwrap();
        let caller = agent.clone();
        let waiter = tokio::spawn(async move { caller.shutdown().await });
        tokio::time::timeout(Duration::from_secs(10), started)
            .await
            .unwrap()
            .unwrap();
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        assert!(!receipt.exists());
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(10), agent.shutdown())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(receipt).unwrap(),
            "cleanup completed once"
        );
        assert!(
            agent
                .prompt("stopped despite cancelled waiter")
                .await
                .is_err()
        );
        eprintln!(
            "family={family} cancelled-shutdown-waiter=isolated later-shutdown=joins-cleanup-receipt"
        );
    }
}
