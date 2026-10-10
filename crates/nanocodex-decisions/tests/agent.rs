//! Agent journeys through the installed decision tool.
//!
//! Real Responses and Claude agents call `decide`: the Responses agent from
//! Code Mode, the Claude agent as a native tool. Only the provider HTTP APIs
//! are local fixtures: scripted models that turn each prompt into one tool
//! call and repeat its output as their final message, and a Decisions
//! endpoint with canned answers. The agent loops, Code Mode runtime, tool
//! registries, and Decisions client run unmodified.

use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use nanocodex::claude::{Claude, ClaudeClient, ClaudeTools};
use nanocodex::{Nanocodex, OpenAi, Tools, agent::AgentEvents, oai::transport::ResponsesTransport};
use nanocodex_decisions::{DecisionTool, openai::OpenAiDecisions};
use serde_json::{Value, json};

/// Every request either fixture received, in arrival order.
type Transcript = Arc<Mutex<Vec<Value>>>;

struct Fixture {
    address: SocketAddr,
    transcript: Transcript,
    trace: PathBuf,
}

impl Fixture {
    async fn start(journey: &str) -> Self {
        // The fixture's Claude client is a bare reqwest client, which needs a
        // process-wide TLS provider.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let trace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../output/decisions")
            .join(format!("{journey}-{}.json", std::process::id()));
        std::fs::create_dir_all(trace.parent().unwrap()).unwrap();
        let transcript = Transcript::default();
        let record = |transcript: &Transcript, entry: Value| {
            transcript.lock().unwrap().push(entry);
        };
        let responses_log = Arc::clone(&transcript);
        let messages_log = Arc::clone(&transcript);
        let decisions_log = Arc::clone(&transcript);
        let app = Router::new()
            .route(
                "/responses",
                post(move |Json(body): Json<Value>| {
                    record(&responses_log, json!({ "api": "responses", "body": body }));
                    async move {
                        (
                            [("content-type", "text/event-stream")],
                            scripted_responses(&body),
                        )
                    }
                }),
            )
            .route(
                "/v1/messages",
                post(move |Json(body): Json<Value>| {
                    record(&messages_log, json!({ "api": "messages", "body": body }));
                    async move {
                        (
                            [("content-type", "text/event-stream")],
                            scripted_messages(&body),
                        )
                    }
                }),
            )
            .route(
                "/v1/decisions",
                post(move |headers: HeaderMap, Json(body): Json<Value>| {
                    let authorization = headers["authorization"].to_str().unwrap().to_owned();
                    record(
                        &decisions_log,
                        json!({ "api": "decisions", "authorization": authorization, "body": body }),
                    );
                    async move { decisions_api(&body) }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            address,
            transcript,
            trace,
        }
    }

    /// Builds a Responses agent with only the decision tool installed.
    fn codex_agent(&self) -> (Nanocodex, AgentEvents) {
        let openai = OpenAi::builder("synthetic-responses-key")
            .transport(ResponsesTransport::Https)
            .store(false)
            .api_base_url(format!("http://{}", self.address))
            .build()
            .unwrap();
        let tools = Tools::builder()
            .without_defaults()
            .tool(DecisionTool::new(self.decisions()))
            .build()
            .unwrap();
        Nanocodex::builder(openai).tools(tools).build().unwrap()
    }

    /// Builds a Claude agent with only the decision tool installed.
    fn claude_agent(&self) -> (Nanocodex, AgentEvents) {
        let client = ClaudeClient::new(
            reqwest::Client::new(),
            format!("http://{}/v1/messages", self.address),
            "synthetic-claude-key",
        );
        let decisions = self.decisions();
        Nanocodex::builder(Claude::new(client, "claude-opus-5-5"))
            .tools_factory(move |_handle| {
                ClaudeTools::new().shared_tool(DecisionTool::new(decisions.clone()))
            })
            .build()
            .unwrap()
    }

    fn decisions(&self) -> OpenAiDecisions {
        OpenAiDecisions::builder("synthetic-decisions-key")
            .base_url(format!("http://{}/v1/", self.address))
            .build()
    }

    /// Runs one prompt, which the scripted model turns into a single tool
    /// call, and returns the tool's output.
    async fn run(&self, agent: &Nanocodex, prompt: &str) -> String {
        let turn = agent.prompt(prompt).await.unwrap();
        let result = turn.result().await;
        self.save();
        result.unwrap().final_message().to_owned()
    }

    fn requests(&self, api: &str) -> Vec<Value> {
        self.transcript
            .lock()
            .unwrap()
            .iter()
            .filter(|entry| entry["api"] == api)
            .cloned()
            .collect()
    }

    fn save(&self) {
        let transcript = self.transcript.lock().unwrap();
        std::fs::write(
            &self.trace,
            serde_json::to_vec_pretty(&*transcript).unwrap(),
        )
        .unwrap();
        println!("TRANSCRIPT {}", self.trace.display());
    }
}

/// Runs the user's prompt as an `exec` cell, then repeats the cell's text
/// output.
fn scripted_responses(body: &Value) -> String {
    let last = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|item| item["type"] == "custom_tool_call_output" || item["role"] == "user")
        .unwrap();
    let item = if last["type"] == "custom_tool_call_output" {
        let text: String = last["output"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect();
        json!({
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": text }]
        })
    } else {
        json!({
            "type": "custom_tool_call",
            "call_id": "call-1",
            "name": "exec",
            "input": last["content"][0]["text"]
        })
    };
    let frame = json!({
        "type": "response.completed",
        "response": { "id": "response-1", "status": "completed", "output": [item] }
    });
    format!("data: {frame}\n\ndata: [DONE]\n\n")
}

/// Canned Decisions API behavior. Text inputs select a failure or a single
/// Calls `decide` with the user's prompt as its JSON arguments, then repeats
/// the tool result, prefixed with `error: ` when Claude marked it as failed.
fn scripted_messages(body: &Value) -> String {
    let messages = body["messages"].as_array().unwrap();
    let content = &messages.last().unwrap()["content"];
    let result = content
        .as_array()
        .and_then(|blocks| blocks.iter().find(|block| block["type"] == "tool_result"));
    let block = match result {
        Some(result) => {
            let text = result["content"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| result["content"][0]["text"].as_str().unwrap().to_owned());
            let prefix = if result["is_error"] == true {
                "error: "
            } else {
                ""
            };
            json!({ "type": "text", "text": format!("{prefix}{text}") })
        }
        None => {
            let prompt = content
                .as_str()
                .or_else(|| content[0]["text"].as_str())
                .unwrap();
            json!({
                "type": "tool_use",
                "id": format!("toolu_{}", messages.len()),
                "name": "decide",
                "input": serde_json::from_str::<Value>(prompt).unwrap()
            })
        }
    };
    let stop = if block["type"] == "tool_use" {
        "tool_use"
    } else {
        "end_turn"
    };
    let start = if block["type"] == "tool_use" {
        json!({ "type": "tool_use", "id": block["id"], "name": "decide", "input": {} })
    } else {
        block.clone()
    };
    let mut frames = vec![
        json!({
            "type": "message_start",
            "message": {
                "id": "msg_1", "role": "assistant", "model": body["model"], "content": [],
                "usage": { "input_tokens": 10, "output_tokens": 0 }
            }
        }),
        json!({ "type": "content_block_start", "index": 0, "content_block": start }),
    ];
    if block["type"] == "tool_use" {
        frames.push(json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "input_json_delta", "partial_json": block["input"].to_string() }
        }));
    }
    frames.extend([
        json!({ "type": "content_block_stop", "index": 0 }),
        json!({
            "type": "message_delta",
            "delta": { "stop_reason": stop },
            "usage": { "output_tokens": 5 }
        }),
        json!({ "type": "message_stop" }),
    ]);
    frames
        .iter()
        .map(|frame| format!("data: {frame}\n\n"))
        .collect()
}

/// Canned Decisions API behavior. Text inputs select a failure or a single
/// predicate answer; multimodal input receives answers to the four-question
/// journey.
fn decisions_api(body: &Value) -> (StatusCode, Json<Value>) {
    let usage = json!({
        "input_tokens": 42,
        "input_tokens_details": { "cached_tokens": 0, "cache_write_tokens": 0 },
        "output_tokens": 0,
        "output_tokens_details": { "reasoning_tokens": 0 },
        "total_tokens": 42
    });
    match body["input"].as_str() {
        Some("rate limited") => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(
                json!({ "error": { "message": "Rate limit reached for gpt-6-luna", "type": "requests" } }),
            ),
        ),
        Some("unrelated answer") => (
            StatusCode::OK,
            Json(json!({
                "model": "gpt-6-luna",
                "answers": [{ "type": "predicate", "name": "something_else", "probability": 0.5 }],
                "usage": usage
            })),
        ),
        Some(_) => (
            StatusCode::OK,
            Json(json!({
                "model": "gpt-6-luna",
                "answers": [{ "type": "predicate", "name": "damaged", "probability": 0.97 }],
                "usage": usage
            })),
        ),
        _ => (
            StatusCode::OK,
            Json(json!({
                "model": "gpt-6-luna",
                "answers": [
                    { "type": "predicate", "name": "damaged", "probability": 0.92 },
                    {
                        "type": "choice",
                        "name": "department",
                        "choice": "billing",
                        "probabilities": [
                            { "value": "billing", "probability": 0.95 },
                            { "value": "other", "probability": 0.05 }
                        ],
                        "confidence": 0.93
                    },
                    {
                        "type": "score",
                        "name": "severity",
                        "score": 1.1,
                        "probabilities": [
                            { "value": 0, "label": "Cosmetic", "probability": 0.1 },
                            { "value": 1, "label": "Workaround available", "probability": 0.7 },
                            { "value": 2, "label": "Fully blocked", "probability": 0.2 }
                        ],
                        "confidence": 0.55
                    },
                    { "type": "refusal", "name": "intent" }
                ],
                "usage": usage
            })),
        ),
    }
}

/// Code Mode source that prints the tool's JSON result or its error.
fn decide(arguments: &Value) -> String {
    format!(
        "try {{ text(JSON.stringify(await tools.decide({arguments}))); }} \
         catch (error) {{ text('decide failed: ' + String(error)); }}"
    )
}

/// The text a cell printed, after Code Mode's status header.
fn printed(output: &str) -> &str {
    output.split_once("Output:\n").map_or_else(
        || panic!("unexpected exec output: {output}"),
        |(_, printed)| printed,
    )
}

const IMAGE: &str = "data:image/png;base64,iVBORw0KGgo=";

/// Arguments asking every question type about a photo and its caption.
fn multimodal_request() -> Value {
    json!({
        "input": [
            { "type": "input_text", "text": "Customer photo of a returned phone." },
            { "type": "input_image", "image_url": IMAGE, "detail": "high" }
        ],
        "questions": [
            { "type": "predicate", "name": "damaged", "instructions": "Is the screen cracked?" },
            {
                "type": "choice",
                "name": "department",
                "instructions": "Who handles this return?",
                "choices": [
                    { "value": "billing", "description": "Refunds." },
                    { "value": "other" }
                ]
            },
            {
                "type": "score",
                "name": "severity",
                "instructions": "How severe is the damage?",
                "levels": [
                    { "label": "Cosmetic" },
                    { "label": "Workaround available" },
                    { "label": "Fully blocked", "description": "Unusable." }
                ]
            },
            { "type": "predicate", "name": "intent", "instructions": "Is this fraud?" }
        ]
    })
}

/// Asserts that the Decisions API received [`multimodal_request`] in OpenAI's
/// wire format: the parts become one user message and the questions keep
/// their shape.
fn assert_multimodal_wire(fixture: &Fixture) {
    let decisions = fixture.requests("decisions");
    assert_eq!(decisions.len(), 1);
    assert_eq!(
        decisions[0]["authorization"],
        "Bearer synthetic-decisions-key"
    );
    let request = multimodal_request();
    assert_eq!(
        decisions[0]["body"],
        json!({
            "model": "gpt-6-luna",
            "input": [{ "role": "user", "content": request["input"] }],
            "questions": request["questions"]
        })
    );
}

/// The provider-neutral result the agent receives for [`multimodal_request`].
fn multimodal_decision() -> Value {
    json!({
        "model": "gpt-6-luna",
        "answers": [
            { "name": "damaged", "type": "predicate", "probability": 0.92 },
            {
                "name": "department",
                "type": "choice",
                "choice": "billing",
                "confidence": 0.93,
                "probabilities": [
                    { "value": "billing", "probability": 0.95 },
                    { "value": "other", "probability": 0.05 }
                ]
            },
            {
                "name": "severity",
                "type": "score",
                "score": 1.1,
                "confidence": 0.55,
                "probabilities": [
                    { "index": 0, "label": "Cosmetic", "probability": 0.1 },
                    { "index": 1, "label": "Workaround available", "probability": 0.7 },
                    { "index": 2, "label": "Fully blocked", "probability": 0.2 }
                ]
            },
            { "name": "intent", "type": "refusal" }
        ],
        "usage": { "input_tokens": 42, "output_tokens": 0 }
    })
}

#[tokio::test]
async fn agent_asks_every_question_type_and_reads_typed_answers() {
    let fixture = Fixture::start("answers").await;
    let (agent, _events) = fixture.codex_agent();

    let output = fixture.run(&agent, &decide(&multimodal_request())).await;

    // The model saw the installed tool's typed declaration in the Code Mode
    // catalog.
    let first_turn = &fixture.requests("responses")[0]["body"];
    let exec = first_turn["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "additional_tools")
        .flat_map(|item| item["tools"].as_array().unwrap())
        .find(|tool| tool["name"] == "exec")
        .expect("exec is model-visible");
    let catalog = exec["description"].as_str().unwrap();
    assert!(
        catalog.contains("declare const tools: { decide(args: {"),
        "{catalog}"
    );
    assert_multimodal_wire(&fixture);
    assert_eq!(
        serde_json::from_str::<Value>(printed(&output)).unwrap(),
        multimodal_decision()
    );
    agent.shutdown().await.unwrap();
}

#[tokio::test]
async fn agent_sees_failures_and_recovers_with_a_corrected_call() {
    let fixture = Fixture::start("failures").await;
    let (agent, _events) = fixture.codex_agent();
    let predicate = |input: &str| {
        json!({
            "input": input,
            "questions": [{ "type": "predicate", "name": "damaged", "instructions": "Damaged?" }]
        })
    };

    // Invalid arguments fail before any Decisions request is sent.
    let output = fixture
        .run(
            &agent,
            &decide(&json!({
                "input": "A cracked screen.",
                "questions": [
                    { "type": "predicate", "name": "damaged", "instructions": "Damaged?" },
                    { "type": "predicate", "name": "damaged", "instructions": "Broken?" }
                ]
            })),
        )
        .await;
    assert_eq!(
        printed(&output),
        "decide failed: failed to parse function arguments: invalid decision request: \
         question name `damaged` is used more than once"
    );
    assert!(fixture.requests("decisions").is_empty());

    // An API error reaches the agent with its status and message.
    let output = fixture
        .run(&agent, &decide(&predicate("rate limited")))
        .await;
    assert_eq!(
        printed(&output),
        "decide failed: decision API returned HTTP 429: Rate limit reached for gpt-6-luna"
    );

    // A response that does not answer the question is rejected.
    let output = fixture
        .run(&agent, &decide(&predicate("unrelated answer")))
        .await;
    assert_eq!(
        printed(&output),
        "decide failed: invalid decision response: \
         expected an answer to `damaged` but received `something_else`"
    );

    // The same session keeps working after the failures. A structured record
    // reaches OpenAI as compact JSON text.
    let output = fixture
        .run(
            &agent,
            &decide(&json!({
                "input": { "ticket": 7, "body": "A cracked screen." },
                "questions": [{ "type": "predicate", "name": "damaged", "instructions": "Damaged?" }]
            })),
        )
        .await;
    let decision: Value = serde_json::from_str(printed(&output)).unwrap();
    assert_eq!(
        decision["answers"],
        json!([{ "name": "damaged", "type": "predicate", "probability": 0.97 }])
    );
    let decisions = fixture.requests("decisions");
    assert_eq!(decisions.len(), 3);
    assert_eq!(
        decisions[2]["body"]["input"],
        r#"{"body":"A cracked screen.","ticket":7}"#
    );
    agent.shutdown().await.unwrap();
}

#[tokio::test]
async fn claude_agent_calls_the_same_tool_natively() {
    let fixture = Fixture::start("claude").await;
    let (agent, _events) = fixture.claude_agent();

    // Invalid arguments fail before any Decisions request is sent.
    let output = fixture
        .run(
            &agent,
            &json!({
                "input": "A cracked screen.",
                "questions": [
                    { "type": "predicate", "name": "damaged", "instructions": "Damaged?" },
                    { "type": "predicate", "name": "damaged", "instructions": "Broken?" }
                ]
            })
            .to_string(),
        )
        .await;
    assert_eq!(
        output,
        "error: failed to parse function arguments: invalid decision request: \
         question name `damaged` is used more than once"
    );
    assert!(fixture.requests("decisions").is_empty());

    let output = fixture.run(&agent, &multimodal_request().to_string()).await;

    // Claude saw the tool with its input schema and, because Claude tools
    // have no output schema field, the result shape in its description.
    let tools = fixture.requests("messages")[0]["body"]["tools"].clone();
    let decide = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "decide")
        .expect("decide is model-visible");
    assert_eq!(
        decide["input_schema"]["required"],
        json!(["input", "questions"])
    );
    assert!(
        decide["description"]
            .as_str()
            .unwrap()
            .contains("Output schema: {"),
        "{decide}"
    );
    assert_multimodal_wire(&fixture);
    assert_eq!(
        serde_json::from_str::<Value>(&output).unwrap(),
        multimodal_decision()
    );
    agent.shutdown().await.unwrap();
}
