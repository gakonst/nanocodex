use super::*;

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use nanocodex_agent::AgentEvents;
use nanocodex_tools::{
    Tool, ToolContext, ToolDefinition, ToolInput, ToolOutput, ToolResult, contract::async_trait,
};
use tokio::sync::{Barrier, Semaphore};

struct OrderedProbeState {
    rendezvous: Barrier,
    release_first: Semaphore,
    active: AtomicUsize,
    maximum: AtomicUsize,
}

struct OrderedProbe {
    name: &'static str,
    output: &'static str,
    blocks: bool,
    state: Arc<OrderedProbeState>,
}

#[async_trait]
impl Tool for OrderedProbe {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            self.name,
            "Deterministic parallel-dispatch probe.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        )
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        true
    }

    async fn execute(&self, _input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let active = self.state.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.state.maximum.fetch_max(active, Ordering::SeqCst);
        self.state.rendezvous.wait().await;
        if self.blocks {
            let permit = self.state.release_first.acquire().await?;
            permit.forget();
        } else {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
        self.state.active.fetch_sub(1, Ordering::SeqCst);
        Ok(ToolOutput::text(self.output))
    }
}

fn ordered_tools(state: &Arc<OrderedProbeState>) -> Result<Tools> {
    Ok(Tools::builder()
        .without_defaults()
        .tool(OrderedProbe {
            name: "parallel__first",
            output: "first-ok",
            blocks: true,
            state: Arc::clone(state),
        })
        .tool(OrderedProbe {
            name: "parallel__second",
            output: "second-ok",
            blocks: false,
            state: Arc::clone(state),
        })
        .build()?)
}

fn ordered_calls() -> [Value; 2] {
    [
        json!({
            "type": "function_call",
            "call_id": "call-first",
            "namespace": "parallel__",
            "name": "first",
            "arguments": "{}"
        }),
        json!({
            "type": "function_call",
            "call_id": "call-second",
            "namespace": "parallel__",
            "name": "second",
            "arguments": "{}"
        }),
    ]
}

async fn next_tool_result(events: &mut AgentEvents) -> Result<Value> {
    loop {
        let event = events
            .recv()
            .await
            .ok_or_else(|| eyre!("agent event stream closed before tool result"))?;
        if event.kind == AgentEventKind::ToolResult {
            return Ok(event.decode_payload()?);
        }
    }
}

async fn next_run_completed(events: &mut AgentEvents) -> Result<Value> {
    loop {
        let event = events
            .recv()
            .await
            .ok_or_else(|| eyre!("agent event stream closed before run completion"))?;
        if event.kind == AgentEventKind::RunCompleted {
            return Ok(event.decode_payload()?);
        }
    }
}

#[tokio::test]
async fn parallel_results_emit_on_completion_but_enter_history_in_provider_order() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        let warmup = next_json(&mut socket).await?;
        assert_eq!(warmup["parallel_tool_calls"], false);
        send_warmup(&mut socket, "resp-warmup").await?;

        let _generation = next_json(&mut socket).await?;
        send_json(
            &mut socket,
            completed_response("resp-tools", &ordered_calls()),
        )
        .await?;

        let continuation = next_json(&mut socket).await?;
        let input = continuation["input"]
            .as_array()
            .ok_or_else(|| eyre!("tool continuation input was not an array"))?;
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["call_id"], "call-first");
        assert_eq!(input[0]["output"], "first-ok");
        assert_eq!(input[1]["call_id"], "call-second");
        assert_eq!(input[1]["output"], "second-ok");
        send_final(&mut socket, "resp-final").await
    });

    let state = Arc::new(OrderedProbeState {
        rendezvous: Barrier::new(2),
        release_first: Semaphore::new(0),
        active: AtomicUsize::new(0),
        maximum: AtomicUsize::new(0),
    });
    let workspace = temporary_workspace("parallel-provider-order")?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(&endpoint)
        .build()?;
    let (agent, mut events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .tools(ordered_tools(&state)?)
        .build()?;

    let turn = agent.prompt("Run both probes.").await?;
    let second = timeout(
        std::time::Duration::from_secs(5),
        next_tool_result(&mut events),
    )
    .await
    .map_err(|_| eyre!("second tool did not complete while the first was blocked"))??;
    assert_eq!(second["call_id"], "call-second");
    assert_eq!(second["status"], "completed");
    let second_duration = second["duration_ns"]
        .as_u64()
        .ok_or_else(|| eyre!("second tool duration was missing"))?;
    assert_eq!(state.maximum.load(Ordering::SeqCst), 2);

    state.release_first.add_permits(1);
    assert_eq!(turn.result().await?.final_message(), "done");
    let first = next_tool_result(&mut events).await?;
    assert_eq!(first["call_id"], "call-first");
    assert_eq!(first["status"], "completed");
    let first_duration = first["duration_ns"]
        .as_u64()
        .ok_or_else(|| eyre!("first tool duration was missing"))?;
    let terminal = next_run_completed(&mut events).await?;
    let work = terminal["tool_work_duration_ns"]
        .as_u64()
        .ok_or_else(|| eyre!("parallel tool work duration was missing"))?;
    let wall = terminal["tool_wall_duration_ns"]
        .as_u64()
        .ok_or_else(|| eyre!("parallel tool wall duration was missing"))?;
    assert!(
        work > wall,
        "parallel work {work} should exceed wall {wall}"
    );
    assert!(
        wall < first_duration + second_duration,
        "batch wall {wall} must not sum overlapping call durations"
    );

    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    agent.shutdown().await?;
    drop((agent, events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn cancellation_keeps_completed_siblings_and_aborts_only_pending_calls() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        let _warmup = next_json(&mut socket).await?;
        send_warmup(&mut socket, "resp-warmup").await?;
        let _generation = next_json(&mut socket).await?;
        send_json(
            &mut socket,
            completed_response("resp-tools", &ordered_calls()),
        )
        .await?;

        let (replacement, _) = listener.accept().await?;
        let mut replacement = accept_async(replacement).await?;
        let replay = next_json(&mut replacement).await?;
        assert!(replay.get("previous_response_id").is_none());
        let outputs = replay["input"]
            .as_array()
            .ok_or_else(|| eyre!("replay input was not an array"))?
            .iter()
            .filter(|item| item["type"] == "function_call_output")
            .collect::<Vec<_>>();
        assert_eq!(outputs.len(), 2, "{replay}");
        assert_eq!(outputs[0]["call_id"], "call-first");
        assert!(
            outputs[0]["output"]
                .as_str()
                .is_some_and(|output| output.starts_with("aborted by user after ")),
            "{replay}"
        );
        assert_eq!(outputs[1]["call_id"], "call-second");
        assert_eq!(outputs[1]["output"], "second-ok");
        send_final(&mut replacement, "resp-final").await
    });

    let state = Arc::new(OrderedProbeState {
        rendezvous: Barrier::new(2),
        release_first: Semaphore::new(0),
        active: AtomicUsize::new(0),
        maximum: AtomicUsize::new(0),
    });
    let workspace = temporary_workspace("parallel-cancel")?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(&endpoint)
        .build()?;
    let (agent, mut events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .tools(ordered_tools(&state)?)
        .build()?;

    let cancelled = agent.prompt("Run and cancel both probes.").await?;
    let completed = timeout(
        std::time::Duration::from_secs(5),
        next_tool_result(&mut events),
    )
    .await
    .map_err(|_| eyre!("second tool did not complete before cancellation"))??;
    assert_eq!(completed["call_id"], "call-second");
    assert_eq!(completed["status"], "completed");
    cancelled.cancel().await?;
    assert!(matches!(
        cancelled.result().await,
        Err(NanocodexError::TurnCancelled)
    ));

    let aborted = next_tool_result(&mut events).await?;
    assert_eq!(aborted["call_id"], "call-first");
    assert_eq!(aborted["status"], "cancelled");
    assert!(
        aborted["result"]
            .as_str()
            .is_some_and(|output| output.starts_with("aborted by user after ")),
        "{aborted}"
    );

    assert_eq!(
        agent
            .prompt("Continue after cancellation.")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    agent.shutdown().await?;
    drop(agent);

    while let Some(event) = events.recv().await {
        if event.kind == AgentEventKind::ToolResult {
            let result = event.decode_payload::<Value>()?;
            return Err(eyre!("unexpected duplicate tool result: {result}"));
        }
    }
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

struct ExclusionState {
    active_safe: AtomicUsize,
    unsafe_active: AtomicBool,
    overlap: AtomicBool,
}

struct ExclusionProbe {
    name: &'static str,
    parallel_safe: bool,
    state: Arc<ExclusionState>,
}

#[async_trait]
impl Tool for ExclusionProbe {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            self.name,
            "Checks the top-level parallel safety gate.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        )
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        self.parallel_safe
    }

    async fn execute(&self, _input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        if self.parallel_safe {
            if self.state.unsafe_active.load(Ordering::SeqCst) {
                self.state.overlap.store(true, Ordering::SeqCst);
            }
            self.state.active_safe.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            if self.state.unsafe_active.load(Ordering::SeqCst) {
                self.state.overlap.store(true, Ordering::SeqCst);
            }
            self.state.active_safe.fetch_sub(1, Ordering::SeqCst);
        } else {
            if self.state.unsafe_active.swap(true, Ordering::SeqCst)
                || self.state.active_safe.load(Ordering::SeqCst) != 0
            {
                self.state.overlap.store(true, Ordering::SeqCst);
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            if self.state.active_safe.load(Ordering::SeqCst) != 0 {
                self.state.overlap.store(true, Ordering::SeqCst);
            }
            self.state.unsafe_active.store(false, Ordering::SeqCst);
        }
        Ok(ToolOutput::text(self.name))
    }
}

#[tokio::test]
async fn unsafe_tool_calls_exclude_parallel_safe_siblings() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = accept_async(stream).await?;
        let _warmup = next_json(&mut socket).await?;
        send_warmup(&mut socket, "resp-warmup").await?;
        let _generation = next_json(&mut socket).await?;
        send_json(
            &mut socket,
            completed_response(
                "resp-tools",
                &[
                    namespaced_call("safe-first", "safe_first"),
                    namespaced_call("unsafe", "unsafe"),
                    namespaced_call("safe-second", "safe_second"),
                ],
            ),
        )
        .await?;
        let continuation = next_json(&mut socket).await?;
        let input = continuation["input"]
            .as_array()
            .ok_or_else(|| eyre!("tool continuation input was not an array"))?;
        assert_eq!(
            input
                .iter()
                .map(|item| item["call_id"].as_str().unwrap_or_default())
                .collect::<Vec<_>>(),
            ["safe-first", "unsafe", "safe-second"]
        );
        send_final(&mut socket, "resp-final").await
    });

    let state = Arc::new(ExclusionState {
        active_safe: AtomicUsize::new(0),
        unsafe_active: AtomicBool::new(false),
        overlap: AtomicBool::new(false),
    });
    let tools = Tools::builder()
        .without_defaults()
        .tool(ExclusionProbe {
            name: "gate__safe_first",
            parallel_safe: true,
            state: Arc::clone(&state),
        })
        .tool(ExclusionProbe {
            name: "gate__unsafe",
            parallel_safe: false,
            state: Arc::clone(&state),
        })
        .tool(ExclusionProbe {
            name: "gate__safe_second",
            parallel_safe: true,
            state: Arc::clone(&state),
        })
        .build()?;
    let workspace = temporary_workspace("parallel-exclusion")?;
    let openai = OpenAi::builder("test-key")
        .websocket_url(&endpoint)
        .build()?;
    let (agent, mut events) = Nanocodex::builder(openai)
        .thinking(Thinking::Low)
        .workspace(&workspace)
        .session_id(test_session_id())
        .tools(tools)
        .build()?;
    assert_eq!(
        agent
            .prompt("Run the exclusion probes.")
            .await?
            .result()
            .await?
            .final_message(),
        "done"
    );
    assert!(!state.overlap.load(Ordering::SeqCst));
    let terminal = next_run_completed(&mut events).await?;
    let work = terminal["tool_work_duration_ns"]
        .as_u64()
        .ok_or_else(|| eyre!("serial tool work duration was missing"))?;
    let wall = terminal["tool_wall_duration_ns"]
        .as_u64()
        .ok_or_else(|| eyre!("serial tool wall duration was missing"))?;
    assert!(
        work <= wall,
        "serial handler work {work} must fit inside batch wall {wall}"
    );

    timeout(std::time::Duration::from_secs(5), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    agent.shutdown().await?;
    drop((agent, events));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

fn namespaced_call(call_id: &str, name: &str) -> Value {
    json!({
        "type": "function_call",
        "call_id": call_id,
        "namespace": "gate__",
        "name": name,
        "arguments": "{}"
    })
}
