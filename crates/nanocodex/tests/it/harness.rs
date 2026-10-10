//! Real public-library journey. Only the external providers are fixtures;
//! builders, Code Mode, registry admission, eviction and routing run normally.
use axum::{Json, Router, routing::post};
use nanocodex::{
    Claude, ClaudeModel, DurableAgentExt as _, Harness, HarnessFamily, HarnessModel, Model,
    Mutability, Nanocodex, NanocodexError, OpenAi, ReasoningMode, SessionCheckpoint, Thinking,
    agent::{AgentHandle, SpawnOptions},
    claude::{ClaudeClient, ClaudeToolReply, ClaudeTools, ToolResultContent},
    durability::{MemoryStore, SessionRecord, SessionStore},
    oai::transport::ResponsesTransport,
    tools::{ToolContext, Tools, runtime::ToolRuntime},
};
use nanocodex_subagents::{
    AgentId, AgentStatus, AgentTask, MessagePriority, MessagePurpose, Registry, channel,
    install_tools, start_agent_with,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const DEADLINE: Duration = Duration::from_secs(10);
const SUBMIT: &str = "const receipt = await tools.submit_result({output: 'native-result'}); if (!receipt.accepted) throw Error('result was not accepted'); text(receipt);";
type Handles = Arc<Mutex<HashMap<String, AgentHandle>>>;

fn capture(handles: &Handles, handle: &AgentHandle) {
    handles
        .lock()
        .unwrap()
        .insert(handle.session_id().into(), handle.clone());
}

fn registry_tools(handle: AgentHandle, registry: Arc<Registry>) -> Tools {
    install_tools(
        Tools::builder().without_defaults().build().unwrap(),
        handle,
        registry,
    )
    .unwrap()
}

// An embedding-owned native callback to the real shared registry, using the
// same public Code Mode transport a host can expose to either provider.
fn claude_tools(handle: AgentHandle, registry: Arc<Registry>) -> ClaudeTools {
    let tools = registry_tools(handle, registry);
    let runtime = Arc::new(ToolRuntime::new_with_tools(
        std::env::current_dir().unwrap(),
        None,
        None,
        &tools,
    ));
    let definition = serde_json::from_value(json!({
        "name":"exec", "description":"Run the authorized child lifecycle tools.",
        "input_schema":{"type":"object","properties":{"code":{"type":"string"}},"required":["code"],"additionalProperties":false}
    })).unwrap();
    ClaudeTools::new().tool_with_context(definition, move |input, invocation| {
        let runtime = Arc::clone(&runtime);
        async move {
            let context = ToolContext::new(
                &invocation.model,
                &invocation.session_id,
                &invocation.call_id,
                &[],
                4096,
            )
            .with_host_context(invocation.host_context.as_deref())
            .with_instruction_revision(invocation.instruction_revision);
            let execution = runtime
                .execute_code(input["code"].as_str().ok_or("missing code")?, context)
                .await
                .map_err(|error| error.to_string())?;
            let mut reply = ClaudeToolReply::success(ToolResultContent::Text(
                serde_json::to_string(&execution.output).unwrap(),
            ));
            reply.is_error = !execution.success;
            Ok(reply)
        }
    })
}

fn responses(body: &Value, ordinal: usize) -> String {
    let continuation = body["input"]
        .as_array()
        .unwrap()
        .last()
        .is_some_and(|item| {
            item["type"] == "custom_tool_call_output" || item["type"] == "function_call_output"
        });
    let has_exec = body["tools"]
        .as_array()
        .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == "exec"))
        || body["input"].as_array().unwrap().iter().any(|item| {
            item["type"] == "additional_tools"
                && item["tools"]
                    .as_array()
                    .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == "exec"))
        });
    let output = if continuation || !has_exec {
        json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":"native done"}]}])
    } else {
        json!([{"type":"custom_tool_call","call_id":format!("codex-call-{ordinal}"),"name":"exec","input":SUBMIT}])
    };
    let frame = json!({"type":"response.completed","response":{"id":format!("response-{ordinal}"),"status":"completed","output":output}});
    format!("data: {frame}\n\ndata: [DONE]\n\n")
}

fn messages(body: &Value, ordinal: usize) -> String {
    let continuation = body["messages"]
        .as_array()
        .unwrap()
        .last()
        .and_then(|message| message["content"].as_array())
        .is_some_and(|content| content.iter().any(|block| block["type"] == "tool_result"));
    let has_exec = body["tools"]
        .as_array()
        .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == "exec"));
    let terminal = continuation || !has_exec;
    let block = if terminal {
        json!({"type":"text","text":"native done"})
    } else {
        json!({"type":"tool_use","id":format!("claude-call-{ordinal}"),"name":"exec","input":{}})
    };
    let mut frames = vec![
        json!({"type":"message_start","message":{"id":format!("message-{ordinal}"),"role":"assistant","model":body["model"],"content":[],"usage":{"input_tokens":10,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":block}),
    ];
    if !terminal {
        frames.push(json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":json!({"code":SUBMIT}).to_string()}}));
    }
    frames.extend([
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":if terminal {"end_turn"} else {"tool_use"}},"usage":{"output_tokens":3}}),
        json!({"type":"message_stop"}),
    ]);
    frames
        .into_iter()
        .map(|frame| format!("data: {frame}\n\n"))
        .collect()
}

async fn completed(registry: &Registry, session: &str, id: AgentId) {
    let (summaries, timed_out) = registry.wait(session, &[id], DEADLINE).await.unwrap();
    assert!(!timed_out, "child must finish before deadline");
    let observed = serde_json::to_value(&summaries).unwrap();
    assert!(
        matches!(&summaries[0].status, AgentStatus::Completed { output } if output == "native-result"),
        "{observed}"
    );
}

fn task(text: &str) -> AgentTask {
    AgentTask {
        role: "synthetic specialist".into(),
        task: text.into(),
        output_schema: json!({"type":"string"}),
    }
}

#[tokio::test]
async fn native_recipes_share_children_restore_and_fence_stopped_owners() {
    tokio::time::timeout(DEADLINE * 6, journey())
        .await
        .expect("library journey deadline");
}

async fn journey() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let evidence =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../output/library-harness");
    std::fs::create_dir_all(&evidence).unwrap();
    let path = evidence.join(format!("journey-{}.json", std::process::id()));
    std::fs::write(&path, b"[]").unwrap();
    println!("TRANSCRIPT {}", path.display());
    let transcript = Arc::new(Mutex::new(Vec::<Value>::new()));
    let codex_transcript = Arc::clone(&transcript);
    let claude_transcript = Arc::clone(&transcript);
    let codex_trace = path.clone();
    let claude_trace = path;
    let app = Router::new()
        .route(
            "/responses",
            post(move |Json(body): Json<Value>| {
                let transcript = Arc::clone(&codex_transcript);
                let trace = codex_trace.clone();
                async move {
                    let mut transcript = transcript.lock().unwrap();
                    transcript.push(json!({"transport":"responses","request":body}));
                    std::fs::write(&trace, serde_json::to_vec_pretty(&*transcript).unwrap())
                        .unwrap();
                    (
                        [("content-type", "text/event-stream")],
                        responses(&body, transcript.len()),
                    )
                }
            }),
        )
        .route(
            "/v1/messages",
            post(move |Json(body): Json<Value>| {
                let transcript = Arc::clone(&claude_transcript);
                let trace = claude_trace.clone();
                async move {
                    let mut transcript = transcript.lock().unwrap();
                    transcript.push(json!({"transport":"messages","request":body}));
                    std::fs::write(&trace, serde_json::to_vec_pretty(&*transcript).unwrap())
                        .unwrap();
                    (
                        [("content-type", "text/event-stream")],
                        messages(&body, transcript.len()),
                    )
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let openai = OpenAi::builder("synthetic-key")
        .transport(ResponsesTransport::Https)
        .store(false)
        .api_base_url(format!("http://{address}"))
        .build()
        .unwrap();
    let claude = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic-key",
    );
    // Model settings cross the real native public APIs and are verified in
    // provider requests, without installing task-tree tools on these sessions.
    let policy_handles: Handles = Arc::default();
    let captured = Arc::clone(&policy_handles);
    let (luna, _events) = Nanocodex::builder(openai.clone())
        .model(Model::Luna)
        .thinking(Thinking::None)
        .tools_factory(move |handle| {
            capture(&captured, &handle);
            Tools::builder().without_defaults().build()
        })
        .build()
        .unwrap();
    let luna_owner = policy_handles.lock().unwrap()[luna.session_id()].clone();
    let (sol, _events) = luna_owner
        .spawn_with(SpawnOptions::new().model(Model::Sol))
        .await
        .expect("model-only Sol override must use Low, not inherited None");
    let sol_owner = policy_handles.lock().unwrap()[sol.session_id()].clone();
    assert_eq!(
        sol_owner.settings().await.unwrap(),
        (HarnessModel::Codex(Model::Sol), Thinking::Low)
    );
    assert_eq!(
        sol.prompt("codex-model-default")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message(),
        "native done"
    );
    let sol_request = transcript.lock().unwrap().last().unwrap().clone();
    assert_eq!(sol_request["request"]["model"], Model::Sol.as_str());
    assert_eq!(sol_request["request"]["reasoning"]["effort"], "low");
    sol.shutdown().await.unwrap();
    luna.shutdown().await.unwrap();

    let (astra, _events) = Nanocodex::builder(openai.clone())
        .model(Model::Astra)
        .reasoning_mode(ReasoningMode::Pro)
        .build()
        .expect("Astra supports pro reasoning mode");
    astra
        .prompt("astra-pro-policy")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let astra_request = transcript.lock().unwrap().last().unwrap().clone();
    assert_eq!(astra_request["request"]["model"], Model::Astra.as_str());
    assert_eq!(astra_request["request"]["reasoning"]["mode"], "pro");
    astra.shutdown().await.unwrap();

    let (opus, _events) =
        Nanocodex::builder(Claude::new(claude.clone(), ClaudeModel::Opus55.as_str()))
            .thinking(Thinking::Medium)
            .unwrap()
            .build()
            .unwrap();
    opus.set_harness_model(ClaudeModel::Haiku45.into())
        .await
        .unwrap();
    opus.prompt("claude-haiku-policy")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let haiku_request = transcript.lock().unwrap().last().unwrap().clone();
    assert_eq!(
        haiku_request["request"]["model"],
        ClaudeModel::Haiku45.as_str()
    );
    assert!(haiku_request["request"].get("thinking").is_none());
    assert!(haiku_request["request"].get("output_config").is_none());
    let used_snapshot = opus.checkpoint().await.unwrap();
    let used_session = opus.session_id().to_owned();
    opus.shutdown().await.unwrap();
    let (restored, _events) =
        Nanocodex::builder(Claude::new(claude.clone(), ClaudeModel::Haiku45.as_str()))
            .resume(used_snapshot)
            .unwrap()
            .build()
            .unwrap();
    assert_eq!(restored.session_id(), used_session);
    assert!(matches!(
        restored.set_harness_model(ClaudeModel::Opus55.into()).await,
        Err(NanocodexError::InvalidRequest(_))
    ));
    // Effort follows the advertised capability; the model stays locked.
    assert_eq!(restored.capabilities().thinking, Mutability::Anytime);
    restored.set_thinking(Thinking::None).await.unwrap();
    restored
        .prompt("claude-retained-history")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let restored_request = transcript.lock().unwrap().last().unwrap().clone();
    assert!(
        restored_request["request"]["messages"]
            .to_string()
            .contains("claude-haiku-policy")
    );
    assert_eq!(
        restored_request["request"]["model"],
        ClaudeModel::Haiku45.as_str()
    );
    restored.shutdown().await.unwrap();

    let (untouched, _events) =
        Nanocodex::builder(Claude::new(claude.clone(), ClaudeModel::Haiku45.as_str()))
            .thinking(Thinking::None)
            .unwrap()
            .build()
            .unwrap();
    let untouched_snapshot = untouched.checkpoint().await.unwrap();
    untouched.shutdown().await.unwrap();
    let (mutable, _events) =
        Nanocodex::builder(Claude::new(claude.clone(), ClaudeModel::Haiku45.as_str()))
            .resume(untouched_snapshot)
            .unwrap()
            .build()
            .unwrap();
    mutable.set_thinking(Thinking::None).await.unwrap();
    mutable
        .set_harness_model(ClaudeModel::Opus55.into())
        .await
        .expect("untouched restored session must remain mutable");
    mutable
        .prompt("claude-opus-policy")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let opus_request = transcript.lock().unwrap().last().unwrap().clone();
    assert_eq!(
        opus_request["request"]["model"],
        ClaudeModel::Opus55.as_str()
    );
    assert_eq!(opus_request["request"]["thinking"]["type"], "adaptive");
    assert_eq!(opus_request["request"]["output_config"]["effort"], "medium");
    mutable.shutdown().await.unwrap();

    // The `haiku` alias selects Haiku 5.5, which keeps adaptive effort through Max
    // and rejects the ordinary inference mode reserved for Haiku 4.5.
    let haiku: HarnessModel = "haiku".parse().unwrap();
    assert_eq!(haiku, HarnessModel::Claude(ClaudeModel::Haiku55));
    assert!(matches!(
        Nanocodex::builder(Claude::new(claude.clone(), haiku.as_str())).thinking(Thinking::None),
        Err(NanocodexError::InvalidRequest(_))
    ));
    let (opus, _events) =
        Nanocodex::builder(Claude::new(claude.clone(), ClaudeModel::Opus55.as_str()))
            .thinking(Thinking::Max)
            .unwrap()
            .build()
            .unwrap();
    opus.set_harness_model(haiku).await.unwrap();
    opus.prompt("claude-haiku-5-5-policy")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let haiku_request = transcript.lock().unwrap().last().unwrap().clone();
    assert_eq!(haiku_request["request"]["model"], "claude-haiku-5-5");
    assert_eq!(haiku_request["request"]["thinking"]["type"], "adaptive");
    assert_eq!(haiku_request["request"]["output_config"]["effort"], "max");
    assert!(haiku_request["request"].get("speed").is_none());
    opus.shutdown().await.unwrap();
    println!(
        "MODEL_POLICY Luna/None -> Sol/Low; Opus -> Haiku 4.5 disables thinking; untouched restore -> Opus enables thinking; used restore stays locked with native history; Opus/Max -> haiku alias keeps Haiku 5.5 adaptive Max"
    );
    let (registry, control, _updates) = channel(4);
    control.set_max_resident(1);
    let handles: Handles = Arc::default();
    let recipe_calls = Arc::new(AtomicUsize::new(0));
    let restore_calls = Arc::new(AtomicUsize::new(0));
    let harness = Harness::builder()
        .register(HarnessFamily::Codex, {
            let registry = Arc::clone(&registry);
            let handles = Arc::clone(&handles);
            let recipe_calls = Arc::clone(&recipe_calls);
            move |request| {
                let openai = openai.clone();
                let registry = Arc::clone(&registry);
                let handles = Arc::clone(&handles);
                recipe_calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    let HarnessModel::Codex(model) = request.model else {
                        unreachable!()
                    };
                    let mut builder = Nanocodex::builder(openai)
                        .model(model)
                        .thinking(request.thinking)
                        .spawn_factory(request.spawn_factory)
                        .tools_factory(move |handle| {
                            capture(&handles, &handle);
                            Ok(registry_tools(handle, Arc::clone(&registry)))
                        });
                    if let Some(checkpoint) = request.checkpoint {
                        builder = builder.resume(checkpoint)?;
                    }
                    if let Some(state) = request.durable_state {
                        builder = builder.durability(state).await?;
                    }
                    builder.build()
                }
            }
        })
        .register(HarnessFamily::Claude, {
            let registry = Arc::clone(&registry);
            let handles = Arc::clone(&handles);
            let recipe_calls = Arc::clone(&recipe_calls);
            let restore_calls = Arc::clone(&restore_calls);
            move |request| {
                let claude = claude.clone();
                let registry = Arc::clone(&registry);
                let handles = Arc::clone(&handles);
                recipe_calls.fetch_add(1, Ordering::SeqCst);
                if request.checkpoint.is_some() {
                    restore_calls.fetch_add(1, Ordering::SeqCst);
                }
                async move {
                    let mut builder =
                        Nanocodex::builder(Claude::new(claude, request.model.as_str()))
                            .thinking(request.thinking)?
                            .host_context(request.host_context)
                            .spawn_factory(request.spawn_factory)
                            .tools_factory(move |handle| {
                                capture(&handles, &handle);
                                Ok(claude_tools(handle, Arc::clone(&registry)))
                            });
                    if let Some(checkpoint) = request.checkpoint {
                        builder = builder.resume(checkpoint)?;
                    }
                    if let Some(state) = request.durable_state {
                        builder = builder.durability(state).await?;
                    }
                    builder.build()
                }
            }
        })
        .build();

    let (parent, _events) = harness
        .start(HarnessModel::Codex(Model::Sol))
        .await
        .unwrap();
    let session = parent.session_id().to_owned();
    let owner = handles.lock().unwrap()[&session].clone();
    // The handle predates both updates. Omitted child defaults must be live.
    parent.set_harness_model(Model::Luna.into()).await.unwrap();
    parent.set_thinking(Thinking::High).await.unwrap();
    assert_eq!(
        harness
            .spawn_factory()
            .settings(owner.clone())
            .await
            .unwrap(),
        (HarnessModel::Codex(Model::Luna), Thinking::High)
    );
    let (inherited, _events) = owner.spawn().await.unwrap();
    let inherited_handle = handles.lock().unwrap()[inherited.session_id()].clone();
    assert_eq!(
        inherited_handle.settings().await.unwrap(),
        (HarnessModel::Codex(Model::Luna), Thinking::High)
    );
    inherited.shutdown().await.unwrap();

    let options = SpawnOptions::new()
        .harness(HarnessFamily::Claude)
        .harness_model(HarnessModel::Claude(ClaudeModel::Sonnet55));
    let claude_child = start_agent_with(
        &owner,
        &registry,
        &session,
        task("Remember amber-constraint for the later recall."),
        options,
    )
    .await
    .unwrap();
    completed(&registry, &session, claude_child.agent_id).await;
    let directory = registry.directory(&session, true, false).await.unwrap();
    assert_eq!(directory.len(), 1);
    let old_claude_owner = handles
        .lock()
        .unwrap()
        .values()
        .find(|handle| handle.harness_family() == HarnessFamily::Claude)
        .unwrap()
        .clone();
    let claude_session = old_claude_owner.session_id().to_owned();
    let codex_child = start_agent_with(
        &owner,
        &registry,
        &session,
        task("A second native child forces idle eviction."),
        SpawnOptions::new(),
    )
    .await
    .unwrap();
    completed(&registry, &session, codex_child.agent_id).await;
    // Observe eviction through the public weak capability, without inspecting
    // registry internals or relying on a fixed scheduling delay.
    tokio::time::timeout(DEADLINE, async {
        while old_claude_owner.settings().await.is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("idle Claude child should be evicted");
    registry
        .send_message(
            &session,
            claude_child.agent_id,
            MessagePriority::Deferred,
            MessagePurpose::Coordinate,
            None,
            "Recall amber-constraint after restoration.".into(),
        )
        .await
        .unwrap();
    completed(&registry, &session, claude_child.agent_id).await;
    assert_eq!(restore_calls.load(Ordering::SeqCst), 1);
    let restored_owner = handles.lock().unwrap()[&claude_session].clone();
    assert_eq!(restored_owner.session_id(), old_claude_owner.session_id());
    let requests = transcript.lock().unwrap().clone();
    let recall = requests
        .iter()
        .find(|frame| {
            frame["transport"] == "messages"
                && frame["request"]
                    .to_string()
                    .contains("Recall amber-constraint after restoration.")
        })
        .unwrap();
    assert!(
        recall["request"]
            .to_string()
            .contains("Remember amber-constraint"),
        "native transcript must survive eviction"
    );
    assert_eq!(recall["request"]["model"], ClaudeModel::Sonnet55.as_str());
    assert_eq!(
        registry
            .directory(&session, true, false)
            .await
            .unwrap()
            .len(),
        2
    );

    // Route back from the restored native Claude owner into Codex, retaining
    // shared topology rather than creating a second registry for that family.
    let descendant = start_agent_with(
        &restored_owner,
        &registry,
        &claude_session,
        task("A Codex descendant of the restored Claude specialist."),
        SpawnOptions::new()
            .harness(HarnessFamily::Codex)
            .harness_model(HarnessModel::Codex(Model::Sol)),
    )
    .await
    .unwrap();
    completed(&registry, &session, descendant.agent_id).await;
    let directory = registry.directory(&session, true, false).await.unwrap();
    assert_eq!(directory.len(), 3);
    assert_eq!(
        directory
            .iter()
            .find(|entry| entry.agent_id == descendant.agent_id)
            .unwrap()
            .parent_agent_id,
        Some(claude_child.agent_id),
    );

    let before_invalid = recipe_calls.load(Ordering::SeqCst);
    assert!(matches!(
        harness
            .start_with(
                SpawnOptions::new()
                    .harness(HarnessFamily::Claude)
                    .model(Model::Sol)
            )
            .await,
        Err(NanocodexError::InvalidRequest(_))
    ));
    assert!(matches!(
        harness
            .start_with(
                SpawnOptions::new()
                    .harness(HarnessFamily::Claude)
                    .harness_model(HarnessModel::Claude(ClaudeModel::Haiku45))
                    .thinking(Thinking::High)
            )
            .await,
        Err(NanocodexError::InvalidRequest(_))
    ));
    assert_eq!(before_invalid, recipe_calls.load(Ordering::SeqCst));
    assert!(matches!(
        Harness::builder()
            .build()
            .start(HarnessModel::Claude(ClaudeModel::Sonnet55))
            .await,
        Err(NanocodexError::InvalidRequest(_))
    ));
    println!(
        "INVALID_SELECTION family/model mismatch and unsupported effort rejected before recipes"
    );

    // A host reopens a serialized root checkpoint of either family through the
    // same router: identity, model and committed history survive.
    for model in [
        HarnessModel::Codex(Model::Sol),
        HarnessModel::Claude(ClaudeModel::Sonnet55),
    ] {
        let (root, _events) = harness.start(model).await.unwrap();
        let first = root
            .prompt("Remember cobalt-resume for the resumed session.")
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
        let saved = first.checkpoint().unwrap().to_json().unwrap();
        let session_id = root.session_id().to_owned();
        root.shutdown().await.unwrap();
        let checkpoint = SessionCheckpoint::from_json(&saved).unwrap();
        assert_eq!(checkpoint.turn_id(), first.turn_id());
        assert_eq!(checkpoint.model(), model);
        let (resumed, _events) = harness.resume(checkpoint).await.unwrap();
        assert_eq!(resumed.session_id(), session_id);
        assert_eq!(resumed.harness_family(), model.family());
        resumed
            .prompt("Recall cobalt-resume after resuming.")
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
        let recall = transcript
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|frame| {
                frame["request"]
                    .to_string()
                    .contains("Recall cobalt-resume after resuming.")
            })
            .unwrap()
            .clone();
        assert!(
            recall["request"]
                .to_string()
                .contains("Remember cobalt-resume"),
            "{model} resume must replay committed history"
        );
        resumed.shutdown().await.unwrap();
    }
    let mut foreign = serde_json::to_value(
        SessionCheckpoint::from_json(&parent.checkpoint().await.unwrap().to_json().unwrap())
            .unwrap(),
    )
    .unwrap();
    foreign["format"] = json!("nanocodex-session-checkpoint/0");
    assert!(matches!(
        SessionCheckpoint::from_json(&foreign.to_string()),
        Err(NanocodexError::InvalidCheckpoint(_))
    ));
    println!("ROOT_RESUME Codex and Claude roots resumed from JSON checkpoints with history");

    // A host creates catalog entries and reopens them by ID through the same
    // router, whatever their family: the reopened session owns its durable
    // state and keeps identity, model and committed history.
    let store = SessionStore::new(MemoryStore::new().unwrap()).unwrap();
    for model in [
        HarnessModel::Codex(Model::Sol),
        HarnessModel::Claude(ClaudeModel::Sonnet55),
    ] {
        let session_id = nanocodex::oai::session::SessionId::new().to_string();
        drop(
            store
                .session(SessionRecord::root(session_id.clone(), model, None))
                .await
                .unwrap(),
        );
        let (opened, _events) = harness.open(&store, &session_id).await.unwrap();
        assert_eq!(opened.session_id(), session_id);
        assert_eq!(opened.harness_family(), model.family());
        assert_eq!(
            opened
                .persistence()
                .and_then(|persistence| persistence.durable_state_id),
            Some(session_id.clone())
        );
        opened
            .prompt("Remember cobalt-open for the reopened session.")
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
        opened.shutdown().await.unwrap();
        let (reopened, _events) = harness.open(&store, &session_id).await.unwrap();
        assert_eq!(reopened.session_id(), session_id);
        reopened
            .prompt("Recall cobalt-open after reopening.")
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
        let recall = transcript
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|frame| {
                frame["request"]
                    .to_string()
                    .contains("Recall cobalt-open after reopening.")
            })
            .unwrap()
            .clone();
        assert!(
            recall["request"]
                .to_string()
                .contains("Remember cobalt-open"),
            "{model} open must replay durable history"
        );
        reopened.shutdown().await.unwrap();
        assert_eq!(store.load(&session_id).await.unwrap().turns.len(), 2);
    }
    assert!(matches!(
        harness
            .open(
                &store,
                &nanocodex::oai::session::SessionId::new().to_string()
            )
            .await,
        Err(NanocodexError::InvalidRequest(_))
    ));
    println!("OPEN Codex and Claude catalog sessions reopened by ID with durable history");

    let snapshot = {
        let (agent, _events) = owner.spawn_with(options).await.unwrap();
        let snapshot = agent.checkpoint().await.unwrap();
        agent.shutdown().await.unwrap();
        snapshot
    };
    control.close_all(&session).await.unwrap();
    parent.shutdown().await.unwrap();
    let before = (
        recipe_calls.load(Ordering::SeqCst),
        transcript.lock().unwrap().len(),
    );
    assert!(matches!(
        owner.spawn_with(options).await,
        Err(NanocodexError::AgentStopped)
    ));
    assert!(matches!(
        owner.restore_runtime(snapshot.clone(), None).await,
        Err(NanocodexError::AgentStopped)
    ));
    // Calling the exposed factory directly must retain the owner fence too.
    assert!(matches!(
        harness
            .spawn_factory()
            .ensure_available(owner.clone())
            .await,
        Err(NanocodexError::AgentStopped)
    ));
    assert!(matches!(
        harness.spawn_factory().settings(owner.clone()).await,
        Err(NanocodexError::AgentStopped)
    ));
    assert!(matches!(
        harness
            .spawn_factory()
            .spawn(owner.clone(), options, None)
            .await,
        Err(NanocodexError::AgentStopped)
    ));
    assert!(matches!(
        harness.spawn_factory().restore(owner, snapshot, None).await,
        Err(NanocodexError::AgentStopped)
    ));
    assert_eq!(
        before,
        (
            recipe_calls.load(Ordering::SeqCst),
            transcript.lock().unwrap().len()
        )
    );
    println!(
        "OWNER_FENCE stopped owner rejected spawn+restore before recipe/provider; recipes={} restores={}",
        before.0,
        restore_calls.load(Ordering::SeqCst)
    );
    server.abort();
}
