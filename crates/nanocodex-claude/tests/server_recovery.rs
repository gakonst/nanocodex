//! Recovery of provider-side effects through the real streaming HTTP boundary.
//! Failure cases: complete server effect + output cutoff, truncated stream,
//! and cancellation after server execution has been observed. None may retry.
use axum::{Json, Router, body::Body, response::IntoResponse, routing::post};
use futures_util::{StreamExt, stream};
use nanocodex_agent::{Nanocodex, events::AgentEventKind};
use nanocodex_claude::{Claude, ClaudeClient, ServerToolDefinition};
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[derive(Clone, Copy)]
enum Fault {
    Completed,
    RejectedClient,
    Truncated,
    Cancelled,
}

fn frames(events: Vec<Value>) -> String {
    events
        .into_iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect()
}
fn start() -> Value {
    json!({"type":"message_start","message":{"id":"effect-response","role":"assistant","model":"test","content":[],"usage":{"input_tokens":10,"output_tokens":0},"container":{"id":"recovery-container"}}})
}
fn completed_blocks() -> Vec<Value> {
    vec![
        json!({"type":"thinking","thinking":"Run the authorized mutation once","signature":"signed-evidence"}),
        json!({"type":"server_tool_use","id":"server-effect-1","name":"bash_code_execution","input":{"command":"append synthetic receipt"},"opaque":"keep-call"}),
        json!({"type":"bash_code_execution_tool_result","tool_use_id":"server-effect-1","content":{"type":"bash_code_execution_result","stdout":"receipt committed","stderr":"","return_code":0,"content":[]},"opaque":"keep-result"}),
    ]
}
fn completed(blocks: Vec<Value>, stop: &str) -> String {
    let mut events = vec![start()];
    for (index, block) in blocks.into_iter().enumerate() {
        events.push(json!({"type":"content_block_start","index":index,"content_block":block}));
        events.push(json!({"type":"content_block_stop","index":index}));
    }
    events.push(json!({"type":"message_delta","delta":{"stop_reason":stop,"container":{"id":"recovery-container"}},"usage":{"output_tokens":20}}));
    events.push(json!({"type":"message_stop"}));
    frames(events)
}
fn interrupted() -> String {
    frames(vec![
        start(),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"server-effect-1","name":"bash_code_execution","input":{"command":"append synthetic receipt"}}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"server progress observed"}}),
    ])
}
async fn fixture(
    fault: Fault,
) -> (
    ClaudeClient,
    Arc<Mutex<Vec<Value>>>,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let log = Arc::new(Mutex::new(Vec::new()));
    let received = log.clone();
    let effects = Arc::new(AtomicUsize::new(0));
    let counter = effects.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let received = received.clone();
            let counter = counter.clone();
            async move {
                let index = {
                    let mut log = received.lock().unwrap();
                    log.push(body.clone());
                    log.len()
                };
                if std::env::var_os("NANOCLAUDE_SERVER_RECOVERY_TRACE").is_some() {
                    eprintln!("{}", json!({"request_index":index,"request":body}));
                }
                if body["tool_choice"]["type"] == "none" {
                    return (
                        [("content-type", "text/event-stream")],
                        completed(vec![json!({"type":"text","text":"Synthetic receipt committed once; reconcile the existing receipt."})], "end_turn"),
                    ).into_response();
                }
                if index > 1 {
                    return (
                        [("content-type", "text/event-stream")],
                        completed(vec![json!({"type":"text","text":"recovered"})], "end_turn"),
                    )
                        .into_response();
                }
                // This represents an effect committed by the remote server before
                // the client receives the terminal event or interruption.
                counter.fetch_add(1, Ordering::SeqCst);
                match fault {
                    Fault::RejectedClient => {
                        let mut blocks = completed_blocks();
                        blocks.push(json!({"type":"text","text":"large provider explanation ".repeat(4_000)}));
                        // A repeated tool_use id makes the complete response invalid.
                        let call = json!({"type":"tool_use","id":"client-not-dispatched","name":"unregistered","input":{}});
                        blocks.extend([call.clone(), call]);
                        ([("content-type", "text/event-stream")], completed(blocks, "tool_use")).into_response()
                    }
                    Fault::Completed => (
                        [("content-type", "text/event-stream")],
                        completed(completed_blocks(), "max_tokens"),
                    )
                        .into_response(),
                    Fault::Truncated => {
                        ([("content-type", "text/event-stream")], interrupted()).into_response()
                    }
                    Fault::Cancelled => {
                        let chunks = stream::once(async { Ok::<_, Infallible>(interrupted()) })
                            .chain(stream::pending());
                        (
                            [("content-type", "text/event-stream")],
                            Body::from_stream(chunks),
                        )
                            .into_response()
                    }
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (
        ClaudeClient::new(
            reqwest::Client::new(),
            format!("http://{address}/v1/messages"),
            "synthetic",
        ),
        log,
        effects,
        server,
    )
}

#[tokio::test]
async fn completed_server_effect_survives_token_continuation_and_compaction() {
    let (client, log, effects, server) = fixture(Fault::Completed).await;
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .server_tool(ServerToolDefinition::code_execution_current())
        .build()
        .unwrap();
    assert_eq!(
        agent
            .prompt("perform one server effect")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "recovered"
    );
    assert_eq!(
        log.lock().unwrap().len(),
        2,
        "token cutoff continues without replaying server effects"
    );
    assert!(
        log.lock().unwrap()[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["role"] == "assistant"
                && message["content"] == json!(completed_blocks()))
    );
    agent.compact().await.unwrap();
    agent
        .prompt("reconcile the existing receipt")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let log = log.lock().unwrap();
    assert_eq!(log.len(), 4);
    assert_eq!(log[3]["container"], "recovery-container");
    assert!(
        log[2]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["role"] == "assistant"
                && message["content"] == json!(completed_blocks())),
        "summary request must receive the exact completed signed server boundary"
    );
    assert!(
        log[3]["messages"]
            .to_string()
            .contains("Synthetic receipt committed once")
    );
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    server.abort();
}

async fn interrupted_server_effect_is_unknown(fault: Fault) {
    let (client, log, effects, server) = fixture(fault).await;
    let (agent, mut events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .server_tool(ServerToolDefinition::code_execution_current())
        .build()
        .unwrap();
    let turn = agent.prompt("perform one server effect").await.unwrap();
    if matches!(fault, Fault::Cancelled) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let event = events.next().await.unwrap();
                if event.kind == AgentEventKind::AssistantDelta {
                    break;
                }
            }
        })
        .await
        .unwrap();
        turn.cancel().await.unwrap();
    }
    assert!(
        tokio::time::timeout(Duration::from_secs(3), turn.result())
            .await
            .unwrap()
            .is_err()
    );
    assert_eq!(
        log.lock().unwrap().len(),
        1,
        "interruption must not trigger a retry"
    );
    agent
        .prompt("reconcile before doing more work")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let log = log.lock().unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log[1]["container"], "recovery-container");
    let messages = log[1]["messages"].as_array().unwrap();
    let notice = messages
        .iter()
        .filter(|message| message["role"] == "user")
        .flat_map(|message| message["content"].as_array().unwrap())
        .filter_map(|block| block["text"].as_str())
        .find(|text| text.contains("outcome unknown"))
        .expect("next prompt must contain explicit unknown-outcome recovery evidence");
    assert!(notice.contains("server-effect-1"));
    assert!(notice.contains("automatically repeat"));
    assert!(
        !messages
            .iter()
            .any(|message| message["role"] == "assistant"),
        "partial stream must not become a fabricated completed assistant response"
    );
    assert!(
        !log[1]["messages"].to_string().contains("tool_result"),
        "no completed server result may be fabricated"
    );
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn truncated_server_stream_retains_unknown_outcome() {
    interrupted_server_effect_is_unknown(Fault::Truncated).await;
}
#[tokio::test]
async fn cancelled_server_stream_retains_unknown_outcome() {
    interrupted_server_effect_is_unknown(Fault::Cancelled).await;
}

#[tokio::test]
async fn rejected_client_call_retains_prior_server_effect_as_recovery_data() {
    let (client, log, effects, server) = fixture(Fault::RejectedClient).await;
    let (agent, _) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(128_000)
        .server_tool(ServerToolDefinition::code_execution_current())
        .build()
        .unwrap();
    assert!(
        agent
            .prompt("perform one server effect")
            .await
            .unwrap()
            .result()
            .await
            .is_err()
    );
    assert_eq!(
        log.lock().unwrap().len(),
        1,
        "invalid response must not trigger retry"
    );
    agent
        .prompt("reconcile prior server work")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let log = log.lock().unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log[1]["container"], "recovery-container");
    let messages = log[1]["messages"].as_array().unwrap();
    let notice = messages
        .iter()
        .filter(|message| message["role"] == "user")
        .flat_map(|message| message["content"].as_array().unwrap())
        .filter_map(|block| block["text"].as_str())
        .find(|text| text.contains("receipt committed"))
        .expect("validation failure must retain the completed remote receipt");
    assert!(
        notice.len() <= 66_000,
        "invalid provider evidence must have a bounded history footprint"
    );
    assert!(notice.contains("provider content truncated"));
    assert!(notice.contains("no client tools"));
    assert!(notice.contains("automatically repeat"));
    assert!(
        !messages
            .iter()
            .any(|message| message["role"] == "assistant"),
        "malformed mixed response must not be replayed as an unpaired assistant tool call"
    );
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    server.abort();
}
