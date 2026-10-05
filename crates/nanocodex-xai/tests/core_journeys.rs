//! Public lifecycle journeys over real HTTP/SSE. Only the upstream model is scripted.
use axum::{Json, Router, response::IntoResponse, routing::post};
use nanocodex_agent::{
    Nanocodex,
    input::{Prompt, UserInput},
};
use nanocodex_xai::{ToolDefinition, Xai, XaiClient};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
fn response(output: Vec<Value>) -> String {
    format!(
        "data: {}\n\n",
        json!({"type":"response.completed","response":{"status":"completed","output":output,"usage":{"input_tokens":20,"output_tokens":2,"total_tokens":22}}})
    )
}
fn message(text: &str) -> Value {
    json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]})
}
async fn serve_fixture<F>(
    f: F,
) -> (
    XaiClient,
    Arc<Mutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
)
where
    F: Fn(usize, &Value) -> (u16, String) + Send + Sync + 'static,
{
    let _ = rustls::crypto::ring::default_provider().install_default();
    let trace = Arc::new(Mutex::new(Vec::new()));
    let captured = trace.clone();
    let f = Arc::new(f);
    let app = Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let trace = captured.clone();
            let f = f.clone();
            async move {
                let index = {
                    let mut log = trace.lock().unwrap();
                    log.push(body.clone());
                    log.len()
                };
                let (status, body) = f(index, &body);
                (
                    axum::http::StatusCode::from_u16(status).unwrap(),
                    [("content-type", "text/event-stream")],
                    body,
                )
                    .into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    (
        XaiClient::new(
            reqwest::Client::new(),
            format!("http://{address}/responses"),
            "fixture",
        ),
        trace,
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
    )
}
async fn ask(agent: &Nanocodex, text: impl Into<Prompt>) -> nanocodex_agent::TurnResult {
    agent
        .prompt(text.into())
        .await
        .unwrap()
        .result()
        .await
        .unwrap()
}
fn evidence(name: &str, trace: &Arc<Mutex<Vec<Value>>>) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../output/xai");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{name}.json")),
        serde_json::to_vec_pretty(&*trace.lock().unwrap()).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn compact_rollback_native_fork_restore_and_multimodal_context() {
    let summary_attempt = Arc::new(AtomicUsize::new(0));
    let attempts = summary_attempt.clone();
    let (client, trace, server) = serve_fixture(move |_, body| {
        let summary = body["input"][0]["content"]
            .as_str()
            .is_some_and(|s| s.starts_with("Summarize"));
        if summary {
            if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                return (500, "failure".into());
            }
            return (
                200,
                response(vec![message("Earlier task: preserve the violet note.")]),
            );
        }
        (200, response(vec![message("acknowledged")]))
    })
    .await;
    let recipe = Xai::new(client, "grok-4.6")
        .system("stable system")
        .compaction_keep_tail(2);
    let (agent, _) = recipe.clone().build().unwrap();
    let first = ask(
        &agent,
        format!("Remember violet. {}", "historical detail ".repeat(300)),
    )
    .await;
    ask(&agent, "continue current task").await;
    let before = agent.runtime_snapshot().await.unwrap();
    assert!(agent.compact().await.is_err());
    assert_eq!(
        format!("{before:?}"),
        format!("{:?}", agent.runtime_snapshot().await.unwrap())
    );
    agent.compact().await.unwrap();
    let context = agent
        .append_developer_message("keep the answer concise")
        .await
        .unwrap();
    let exported = serde_json::to_value(context.history()).unwrap().to_string();
    assert!(exported.contains("Earlier task"));
    assert!(exported.contains("continue current task"));
    assert!(exported.contains("keep the answer concise"));
    let (fork, _) = agent.fork_from(&first).await.unwrap();
    let prior = serde_json::to_value(fork.context().await.unwrap().history())
        .unwrap()
        .to_string();
    assert!(prior.contains("Remember violet"));
    assert!(!prior.contains("continue current task"));
    let snapshot = agent.runtime_snapshot().await.unwrap();
    let (restored, _) = recipe.restore_runtime(snapshot).unwrap().build().unwrap();
    assert_eq!(
        exported,
        serde_json::to_value(restored.context().await.unwrap().history())
            .unwrap()
            .to_string()
    );
    ask(
        &restored,
        Prompt::content([
            UserInput::Text {
                text: "inspect image".into(),
            },
            UserInput::Image {
                image_url: "data:image/png;base64,AA==".into(),
                detail: None,
            },
        ]),
    )
    .await;
    assert!(
        trace.lock().unwrap().last().unwrap()["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["content"]
                .as_array()
                .is_some_and(|parts| parts.iter().any(|p| p["type"] == "input_image")))
    );
    evidence("core-compact-fork-restore", &trace);
    for a in [agent, fork, restored] {
        a.shutdown().await.unwrap()
    }
    server.abort();
    println!(
        "HTTP journey: failed summary preserves exact snapshot; successful summary+tail, developer context, historical fork, native restore and image prompt verified"
    );
}

#[tokio::test]
async fn context_limit_recovers_once_and_retry_is_bounded() {
    let rejected = Arc::new(AtomicUsize::new(0));
    let count = rejected.clone();
    let (client, trace, server) = serve_fixture(move |index, body| {
        if body["input"][0]["content"]
            .as_str()
            .is_some_and(|s| s.starts_with("Summarize"))
        {
            return (
                200,
                response(vec![message("Retained objective: finish the fixture.")]),
            );
        }
        if index > 1 && count.fetch_add(1, Ordering::SeqCst) == 0 {
            return (
                400,
                json!({"error":{"code":"context_length_exceeded"}}).to_string(),
            );
        }
        (200, response(vec![message("finished")]))
    })
    .await;
    let (agent, _) = Xai::new(client, "grok-4.6")
        .compaction_keep_tail(0)
        .build()
        .unwrap();
    ask(&agent, "past context ".repeat(300)).await;
    assert_eq!(
        ask(&agent, "finish fixture").await.final_message(),
        "finished"
    );
    assert_eq!(trace.lock().unwrap().len(), 4);
    evidence("core-context-recovery", &trace);
    agent.shutdown().await.unwrap();
    server.abort();
    let (client, trace, server) = serve_fixture(|_, _| (503, "retryable".into())).await;
    let (agent, _) = Xai::new(client, "grok-4.6").max_retries(2).build().unwrap();
    assert!(
        agent
            .prompt("fixture")
            .await
            .unwrap()
            .result()
            .await
            .is_err()
    );
    assert_eq!(trace.lock().unwrap().len(), 3);
    evidence("core-bounded-retry", &trace);
    agent.shutdown().await.unwrap();
    server.abort();
    println!(
        "HTTP journey: context overflow -> one summary -> successful continuation; max_retries=2 produces exactly three rejected requests"
    );
}

#[tokio::test]
async fn steering_is_consumed_after_tool_boundary_and_repetition_does_not_repeat_effects() {
    let (client,trace,server)=serve_fixture(|index,_|if index<=4{(200,response(vec![json!({"type":"function_call","call_id":format!("call-{index}"),"name":"effect","arguments":"{}"})]))}else{(200,response(vec![message("finished with steering")]))}).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let count = Arc::new(AtomicUsize::new(0));
    let (e, r, c) = (entered.clone(), release.clone(), count.clone());
    let (agent, _) = Xai::new(client, "grok-4.6")
        .repetition_limit(1)
        .tool(
            ToolDefinition {
                name: "effect".into(),
                description: "fixture".into(),
                parameters: json!({"type":"object"}),
            },
            move |_| {
                let (e, r, c) = (e.clone(), r.clone(), c.clone());
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    e.notify_one();
                    r.notified().await;
                    Ok("done once".into())
                }
            },
        )
        .build()
        .unwrap();
    let turn = agent.prompt("perform effect").await.unwrap();
    entered.notified().await;
    turn.steer_with_id("steer-once".into(), "use violet")
        .await
        .unwrap();
    turn.steer_with_id("steer-once".into(), "use violet")
        .await
        .unwrap();
    release.notify_one();
    assert_eq!(
        turn.result().await.unwrap().final_message(),
        "finished with steering"
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
    {
        let log = trace.lock().unwrap();
        assert_eq!(
            log[1]["input"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|v| v["content"] == "use violet")
                .count(),
            1
        );
    }
    evidence("core-steering-repetition", &trace);
    agent.shutdown().await.unwrap();
    server.abort();
    println!(
        "HTTP journey: identified steer applied once after completed tool, fresh call IDs with repeated identical arguments execute host effect only once"
    );
}

#[tokio::test]
async fn cancelled_turn_discards_pending_steering_before_reuse() {
    let (client, trace, server) = serve_fixture(|index, _| {
        if index == 1 { (200, response(vec![json!({"type":"function_call","call_id":"blocked-call","name":"blocked","arguments":"{}"})])) }
        else { (200, response(vec![message("fresh answer")])) }
    }).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let signal = entered.clone();
    let release = Arc::new(tokio::sync::Notify::new());
    let unblock = release.clone();
    let (agent, _) = Xai::new(client, "grok-4.6")
        .tool(
            ToolDefinition {
                name: "blocked".into(),
                description: "waiting effect".into(),
                parameters: json!({"type":"object"}),
            },
            move |_| {
                let signal = signal.clone();
                let unblock = unblock.clone();
                async move {
                    signal.notify_one();
                    unblock.notified().await;
                    Ok("completed before cancellation released ownership".into())
                }
            },
        )
        .build()
        .unwrap();
    let turn = agent.prompt("old task").await.unwrap();
    entered.notified().await;
    turn.steer_with_id("old-steer".into(), "obsolete steering instruction")
        .await
        .unwrap();
    let (cancelled, ()) = tokio::join!(turn.cancel(), async {
        tokio::task::yield_now().await;
        release.notify_one();
    });
    cancelled.unwrap();
    assert!(turn.result().await.is_err());
    assert_eq!(
        ask(&agent, "fresh task").await.final_message(),
        "fresh answer"
    );
    assert!(
        !trace.lock().unwrap().last().unwrap()["input"]
            .to_string()
            .contains("obsolete steering instruction")
    );
    evidence("core-cancelled-steering", &trace);
    agent.shutdown().await.unwrap();
    server.abort();
    println!(
        "HTTP journey: cancelled blocked effect does not leak queued steering into the next prompt"
    );
}

#[tokio::test]
async fn manual_compaction_is_cancelled_by_shutdown_and_blocks_configuration() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let entered = Arc::new(tokio::sync::Notify::new());
    let signal = entered.clone();
    let app = Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| {
            let signal = signal.clone();
            async move {
                if body["input"][0]["content"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("Summarize"))
                {
                    signal.notify_one();
                    std::future::pending::<()>().await;
                }
                (
                    [("content-type", "text/event-stream")],
                    response(vec![message("ack")]),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (agent, _) = Xai::new(
        XaiClient::new(
            reqwest::Client::new(),
            format!("http://{address}/responses"),
            "fixture",
        ),
        "grok-4.6",
    )
    .compaction_keep_tail(0)
    .build()
    .unwrap();
    ask(&agent, "prior detail ".repeat(200)).await;
    ask(&agent, "current task").await;
    let worker = agent.clone();
    let compaction = tokio::spawn(async move { worker.compact().await });
    entered.notified().await;
    assert!(
        agent
            .set_thinking(nanocodex_agent::Thinking::Low)
            .await
            .is_err()
    );
    assert!(agent.prompt("cannot race compaction").await.is_err());
    tokio::time::timeout(std::time::Duration::from_secs(2), agent.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(compaction.await.unwrap().is_err());
    server.abort();
    println!(
        "HTTP journey: stalled manual summary blocks configuration/new prompt and shutdown cancels it within two seconds"
    );
}

#[tokio::test]
async fn hosted_context_is_lossless_and_incomplete_tools_are_never_dispatched() {
    let hosted = json!({"type":"x_search_call","id":"remote-search","status":"completed","results":[{"text":"remote receipt"}]});
    let native = hosted.clone();
    let (client, trace, server) =
        serve_fixture(move |_, _| (200, response(vec![native.clone(), message("answer")]))).await;
    assert!(
        Xai::new(client.clone(), "grok-4.5")
            .x_search()
            .build()
            .is_err()
    );
    assert!(
        Xai::new(client.clone(), "grok-4.5")
            .web_search()
            .build()
            .is_err()
    );
    assert!(
        Xai::new(client.clone(), "grok-4.6")
            .x_search()
            .tools_factory(|_| Ok(nanocodex_xai::XaiTools::new().tool_with_context(
                ToolDefinition {
                    name: "x_search".into(),
                    description: "collision".into(),
                    parameters: json!({"type":"object"})
                },
                |_, _| async { Ok(nanocodex_xai::XaiToolReply::text("unused")) }
            )))
            .build()
            .is_err()
    );
    let (agent, _) = Xai::new(client, "grok-4.6")
        .x_search()
        .x_search()
        .build()
        .unwrap();
    ask(&agent, "inspect").await;
    assert_eq!(
        trace.lock().unwrap()[0]["tools"],
        json!([{"type":"x_search"}])
    );
    let context = serde_json::to_value(agent.context().await.unwrap().history()).unwrap();
    assert!(context.as_array().unwrap().contains(&hosted));
    ask(&agent, "continue").await;
    assert!(
        trace.lock().unwrap()[1]["input"]
            .as_array()
            .unwrap()
            .contains(&hosted)
    );
    evidence("core-hosted-context", &trace);
    agent.shutdown().await.unwrap();
    server.abort();
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    let (client,trace,server) = serve_fixture(|_,_| (200,format!("data: {}\n\n",json!({"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_prompt_tokens"},"output":[{"type":"function_call","call_id":"partial","name":"effect","arguments":"{}"}]}})))).await;
    let (agent, _) = Xai::new(client, "grok-4.6")
        .tool(
            ToolDefinition {
                name: "effect".into(),
                description: "effect".into(),
                parameters: json!({"type":"object"}),
            },
            move |_| {
                count.fetch_add(1, Ordering::SeqCst);
                async { Ok("wrong".into()) }
            },
        )
        .build()
        .unwrap();
    assert!(
        agent
            .prompt("do effect")
            .await
            .unwrap()
            .result()
            .await
            .is_err()
    );
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert_eq!(trace.lock().unwrap().len(), 1);
    evidence("core-incomplete-tool-veto", &trace);
    agent.shutdown().await.unwrap();
    server.abort();
    println!(
        "HTTP journey: hosted native receipts survive export/replay; incomplete tool output dispatches zero effects and makes no retry"
    );
}

#[tokio::test]
async fn native_child_model_switch_defaults_effort_and_custom_models_restore() {
    let (client, trace, server) = serve_fixture(|_, _| (200, response(vec![message("ack")]))).await;
    let owner = Arc::new(Mutex::new(None));
    let capture = owner.clone();
    let (agent, _) = Xai::new(client.clone(), "grok-4.6")
        .thinking(nanocodex_agent::Thinking::Xhigh)
        .tools_factory(move |handle| {
            *capture.lock().unwrap() = Some(handle);
            Ok(nanocodex_xai::XaiTools::new())
        })
        .build()
        .unwrap();
    let handle = owner.lock().unwrap().clone().unwrap();
    let (child, _) = handle
        .spawn_with(
            nanocodex_agent::SpawnOptions::new()
                .harness_model(nanocodex_agent::XaiModel::Grok45.into()),
        )
        .await
        .unwrap();
    ask(&child, "child task").await;
    assert_eq!(trace.lock().unwrap()[0]["model"], "grok-4.5");
    assert_eq!(trace.lock().unwrap()[0]["reasoning"]["effort"], "high");
    child.shutdown().await.unwrap();
    agent.shutdown().await.unwrap();
    assert!(handle.spawn().await.is_err());
    let (custom, _) = Xai::new(client.clone(), "custom-provider-alias")
        .build()
        .unwrap();
    ask(&custom, "custom history").await;
    let snapshot = custom.runtime_snapshot().await.unwrap();
    custom.shutdown().await.unwrap();
    let (restored, _) = Xai::new(client, "grok-4.6")
        .restore_runtime(snapshot)
        .unwrap()
        .build()
        .unwrap();
    ask(&restored, "custom followup").await;
    assert_eq!(
        trace.lock().unwrap().last().unwrap()["model"],
        "custom-provider-alias"
    );
    assert!(
        trace.lock().unwrap().last().unwrap()["input"]
            .to_string()
            .contains("custom history")
    );
    evidence("core-native-spawn-custom-restore", &trace);
    restored.shutdown().await.unwrap();
    server.abort();
    println!(
        "HTTP journey: Grok46 xhigh -> Grok45 child defaults high; custom native model identity/history survive restore"
    );
}

#[tokio::test]
async fn recovery_requires_an_explicit_rejection_without_observed_output() {
    for (scenario, status, code) in [
        ("misleading-transient-code", 401, "HTTP 503"),
        (
            "misleading-context-code",
            400,
            "not_context_length_exceeded",
        ),
        ("streamed-hosted-effect", 200, ""),
        ("streamed-text", 200, ""),
    ] {
        let (client, trace, server) = serve_fixture(move |index, _| {
            if index == 2 {
                if status != 200 {
                    return (status, json!({"error":{"code":code}}).to_string());
                }
                let observed = if scenario == "streamed-hosted-effect" {
                    json!({"type":"response.output_item.done","output_index":0,
                        "item":{"type":"web_search_call","id":"hosted-effect","status":"completed"}})
                } else {
                    json!({"type":"response.output_text.delta","output_index":0,
                        "item_id":"partial","content_index":0,"delta":"partial output"})
                };
                return (200, format!("data: {observed}\n\ndata: {}\n\n",
                    json!({"type":"response.incomplete","response":{"status":"incomplete",
                        "output":[],"incomplete_details":{"reason":"max_prompt_tokens"}}})));
            }
            (200, response(vec![message("committed answer")]))
        }).await;
        let (agent, _) = Xai::new(client, "grok-4.6")
            .web_search()
            .compaction_keep_tail(0)
            .max_retries(1)
            .build()
            .unwrap();
        ask(&agent, "retained earlier context ".repeat(200)).await;
        let failure = agent
            .prompt("continue the task")
            .await
            .unwrap()
            .result()
            .await;
        assert!(
            failure.is_err(),
            "{scenario} must fail without another request"
        );
        assert_eq!(
            trace.lock().unwrap().len(),
            2,
            "{scenario}: provider text or observed output must not authorize recovery"
        );
        assert_eq!(
            ask(&agent, "explicit followup").await.final_message(),
            "committed answer"
        );
        let log = trace.lock().unwrap().clone();
        assert_eq!(log.len(), 3);
        assert!(
            log[2]["input"]
                .to_string()
                .contains("retained earlier context")
        );
        assert!(!log[2]["input"].to_string().contains("partial output"));
        evidence(scenario, &trace);
        agent.shutdown().await.unwrap();
        server.abort();
        println!(
            "HTTP/SSE recovery guard: {scenario}, 2 requests before explicit followup, retained committed history"
        );
    }
}
