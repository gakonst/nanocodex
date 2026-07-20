use std::{collections::HashSet, path::PathBuf, time::Duration};

use eyre::{Context, Result, bail, eyre};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{net::TcpListener, process::Command, time::timeout};
use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};

const DEFAULT_TURNS: usize = 32;
const DEFAULT_PARALLEL_CALLS: usize = 16;
const PROMPT_SENTINEL: &str = "NANOCODEX_OBSERVABILITY_STRESS_PROMPT_DO_NOT_EXPORT";
const API_KEY_SENTINEL: &str = "stress-secret-api-key-do-not-export";
const REASONING_SENTINEL: &str = "NANOCODEX_VISIBLE_REASONING_SUMMARY";
const REASONING_CONTENT_SENTINEL: &str = "NANOCODEX_HIDDEN_REASONING_CONTENT_DO_NOT_EXPORT";

#[tokio::test]
#[ignore = "manual high-volume observability stress; run `just otel-stress`"]
async fn retained_turns_and_hostile_tools_preserve_trace_topology() -> Result<()> {
    let turns = bounded_env("NANOCODEX_STRESS_TURNS", DEFAULT_TURNS, 1, 100)?;
    let parallel_calls = bounded_env(
        "NANOCODEX_STRESS_PARALLEL_CALLS",
        DEFAULT_PARALLEL_CALLS,
        1,
        128,
    )?;
    let jaeger_url = std::env::var("NANOCODEX_STRESS_JAEGER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:16686".to_owned());
    let otlp_endpoint = std::env::var("NANOCODEX_STRESS_OTLP_ENDPOINT")
        .unwrap_or_else(|_| "http://127.0.0.1:4318".to_owned());
    let export_traces =
        std::env::var("NANOCODEX_STRESS_EXPORT").map_or(true, |value| value != "false");
    if export_traces {
        require_jaeger(&jaeger_url).await?;
    }

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let websocket_url = format!("ws://{}", listener.local_addr()?);
    let server = tokio::spawn(serve_responses(listener, turns, parallel_calls));
    let workspace = temporary_workspace()?;
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/nanocodex-mcp/tests/fixtures/stdio-server.mjs");
    let process_timeout = Duration::from_secs(
        u64::try_from(turns)
            .unwrap_or(u64::MAX)
            .saturating_mul(3)
            .clamp(60, 600),
    );
    let started = std::time::Instant::now();
    let mut command = stress_command(
        &websocket_url,
        &workspace,
        &fixture,
        turns,
        export_traces.then_some(otlp_endpoint.as_str()),
    );
    let output = timeout(process_timeout, command.output())
        .await
        .map_err(|_| eyre!("stress CLI exceeded {process_timeout:?}"))??;

    timeout(Duration::from_secs(10), server)
        .await
        .map_err(|_| eyre!("mock Responses server did not finish"))???;
    let workload_elapsed = started.elapsed();
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;
    if !output.status.success() {
        bail!("stress CLI failed:\n{stderr}\n{stdout}");
    }
    let events = stdout
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let session_id = events
        .first()
        .and_then(|event| event["request_id"].as_str())
        .ok_or_else(|| eyre!("first event had no request_id"))?;
    assert_eq!(event_count(&events, "run.completed", None), turns);
    assert_eq!(event_count(&events, "tool.result", Some("exec")), turns);
    assert_eq!(
        event_count(&events, "tool.result", Some("tool_search")),
        turns
    );
    assert_eq!(
        event_count(&events, "tool.result", Some("mcp__fixture__echo")),
        turns * (parallel_calls + 1)
    );
    assert_eq!(
        event_count(&events, "tool.result", Some("apply_patch")),
        turns
    );
    assert!(
        event_count(&events, "tool.result", Some("exec_command")) >= turns * 3,
        "expected at least three shell calls per turn"
    );
    assert_eq!(
        event_count(&events, "tool.result", Some("write_stdin")),
        turns,
        "every yielded shell command must be resumed through write_stdin"
    );

    if export_traces {
        let traces = wait_for_traces(&jaeger_url, session_id, turns).await?;
        validate_traces(&traces, turns, parallel_calls)?;
        validate_trace_content(&traces)?;
    }
    std::fs::remove_dir_all(workspace)?;
    eprintln!(
        "observability stress passed: turns={turns} parallel_mcp_calls={parallel_calls} events={} exported_traces={} workload_elapsed={workload_elapsed:?} validation_elapsed={:?}",
        events.len(),
        if export_traces { turns } else { 0 },
        started.elapsed().saturating_sub(workload_elapsed)
    );
    Ok(())
}

fn stress_command(
    websocket_url: &str,
    workspace: &std::path::Path,
    fixture: &std::path::Path,
    turns: usize,
    otlp_endpoint: Option<&str>,
) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nanocodex"));
    command
        .arg("--api-key")
        .arg(API_KEY_SENTINEL)
        .arg("--websocket-url")
        .arg(websocket_url)
        .arg("--cwd")
        .arg(workspace)
        .arg("--mcp-stdio")
        .arg("fixture=node")
        .arg("--mcp-arg")
        .arg(format!("fixture={}", fixture.display()))
        .arg("--log-filter")
        .arg("warn");
    if let Some(endpoint) = otlp_endpoint {
        command
            .arg("--otel-filter")
            .arg(
                "warn,nanocodex=info,nanocodex_service=info,nanocodex_tools=info,nanocodex_mcp=info",
            )
            .arg("--otel-endpoint")
            .arg(endpoint)
            .arg("--otel-environment")
            .arg("stress");
    }
    command
        .arg("run")
        .arg("--repeat")
        .arg(turns.to_string())
        .arg(PROMPT_SENTINEL);
    command
}

async fn serve_responses(listener: TcpListener, turns: usize, parallel_calls: usize) -> Result<()> {
    let (stream, _) = listener.accept().await?;
    let mut socket = accept_async(stream).await?;
    let warmup = next_json(&mut socket).await?;
    assert!(warmup["input"].is_array());
    send_completed(&mut socket, "response-warmup", &[]).await?;

    let mut previous_response = "response-warmup".to_owned();
    for turn in 0..turns {
        let generation = next_json(&mut socket).await?;
        assert_eq!(generation["previous_response_id"], previous_response);
        let tool_response = format!("response-tool-{turn}");
        send_completed(
            &mut socket,
            &tool_response,
            &[json!({
                "type": "custom_tool_call",
                "call_id": format!("call-exec-{turn}"),
                "name": "exec",
                "input": stress_source(turn, parallel_calls)
            })],
        )
        .await?;

        let continuation = next_json(&mut socket).await?;
        assert_eq!(continuation["previous_response_id"], tool_response);
        assert_eq!(continuation["input"][0]["type"], "custom_tool_call_output");
        previous_response = format!("response-final-{turn}");
        send_completed(
            &mut socket,
            &previous_response,
            &[
                json!({
                    "type": "reasoning",
                    "summary": [{
                        "type": "summary_text",
                        "text": format!("{REASONING_SENTINEL} turn {turn}")
                    }],
                    "content": [{
                        "type": "reasoning_text",
                        "text": format!("{REASONING_CONTENT_SENTINEL} turn {turn}")
                    }]
                }),
                json!({
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": format!("stress turn {turn} complete") }]
                }),
            ],
        )
        .await?;
    }
    Ok(())
}

fn stress_source(turn: usize, parallel_calls: usize) -> String {
    format!(
        r#"
const found = await tools.tool_search({{ query: "echo message", limit: 8 }});
const remote = found.tools[0].name;
const successful = await Promise.all(Array.from({{ length: {parallel_calls} }}, (_, index) =>
  tools[remote]({{ message: "turn-{turn}-call-" + index }})
));
const failures = await Promise.allSettled([
  tools[remote]({{ message: "__fail__" }}),
  tools.apply_patch("definitely not a patch"),
  tools.exec_command({{ cmd: "exit 23", login: false }}),
  tools.exec_command({{ cmd: "yes x | head -c 65536", login: false, max_output_tokens: 256 }})
]);
const yielded = await tools.exec_command({{
  cmd: "printf start; sleep 1; printf end",
  login: false,
  yield_time_ms: 250
}});
if (!yielded.session_id) {{
  throw new Error("exec_command completed before exercising write_stdin");
}}
const resumed = await tools.write_stdin({{
  session_id: yielded.session_id,
  yield_time_ms: 1000
}});
if (resumed.exit_code !== 0) {{
  throw new Error("write_stdin did not observe a clean process exit");
}}
let unknownRejected = false;
try {{
  await tools.__nanocodex_missing_tool__({{}});
}} catch (_) {{
  unknownRejected = true;
}}
text(JSON.stringify({{
  successful: successful.length,
  rejected: failures.filter((result) => result.status === "rejected").length,
  resumed: resumed.exit_code ?? null,
  unknownRejected
}}));
"#
    )
}

async fn require_jaeger(base_url: &str) -> Result<()> {
    reqwest::Client::new()
        .get(base_url)
        .send()
        .await
        .wrap_err_with(|| format!("Jaeger is unavailable at {base_url}; run `just otel-up`"))?
        .error_for_status()
        .wrap_err("Jaeger UI health check failed")?;
    Ok(())
}

async fn wait_for_traces(base_url: &str, session_id: &str, turns: usize) -> Result<Vec<Value>> {
    let client = reqwest::Client::new();
    let endpoint = format!("{}/api/traces", base_url.trim_end_matches('/'));
    let tags = json!({ "session.id": session_id }).to_string();
    for _ in 0..100 {
        let response = client
            .get(&endpoint)
            .query(&[
                ("service", "nanocodex".to_owned()),
                ("limit", (turns + 8).to_string()),
                ("tags", tags.clone()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        let traces = response["data"].as_array().cloned().unwrap_or_default();
        if traces.len() == turns {
            return Ok(traces);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("Jaeger did not retain all {turns} traces for session {session_id}")
}

fn validate_traces(traces: &[Value], turns: usize, parallel_calls: usize) -> Result<()> {
    assert_eq!(traces.len(), turns);
    let mut trace_ids = HashSet::with_capacity(turns);
    let mut operations = Vec::new();
    let mut error_spans = 0_usize;
    let mut yielded_exec_spans = 0_usize;
    let mut write_stdin_spans = 0_usize;
    for trace in traces {
        let trace_id = trace["traceID"]
            .as_str()
            .ok_or_else(|| eyre!("trace had no traceID"))?;
        assert!(trace_ids.insert(trace_id.to_owned()));
        let spans = trace["spans"]
            .as_array()
            .ok_or_else(|| eyre!("trace {trace_id} had no spans"))?;
        let span_ids = spans
            .iter()
            .filter_map(|span| span["spanID"].as_str())
            .collect::<HashSet<_>>();
        let roots = spans
            .iter()
            .filter(|span| span["references"].as_array().is_none_or(Vec::is_empty))
            .collect::<Vec<_>>();
        assert_eq!(roots.len(), 1, "trace {trace_id} did not have one root");
        assert_eq!(roots[0]["operationName"], "agent.turn");
        for span in spans {
            let operation = span["operationName"]
                .as_str()
                .ok_or_else(|| eyre!("span had no operationName"))?;
            assert_ne!(operation, "agent.session");
            operations.push(operation.to_owned());
            if operation == "code_mode.cell_actor" {
                assert!(has_tag_key(span, "runtime.first_event_ns"));
                assert!(has_tag_key(span, "runtime.event_count"));
                assert!(has_tag_key(span, "host.reused"));
                assert!(has_tag_key(span, "host.wait_ns"));
            }
            if operation == "tool.execute"
                && has_tag(span, "tool.name", "exec_command")
                && has_tag_key(span, "shell.session.id")
            {
                yielded_exec_spans += 1;
                assert!(has_tag_key(span, "process.running"));
                assert!(has_tag_key(span, "tool.output.bytes"));
            }
            if operation == "tool.execute" && has_tag(span, "tool.name", "write_stdin") {
                write_stdin_spans += 1;
                assert!(has_tag_key(span, "process.exit.code"));
                assert!(has_tag_key(span, "process.running"));
                assert!(has_tag_key(span, "tool.output.bytes"));
            }
            for reference in span["references"].as_array().into_iter().flatten() {
                assert_eq!(reference["traceID"], trace_id);
                assert!(
                    reference["spanID"]
                        .as_str()
                        .is_some_and(|parent| span_ids.contains(parent)),
                    "span {operation} in trace {trace_id} had a missing parent"
                );
            }
            if has_tag(span, "otel.status_code", "ERROR") {
                error_spans += 1;
            }
        }
    }
    assert_eq!(operation_count(&operations, "agent.turn"), turns);
    assert_eq!(operation_count(&operations, "tool.call"), turns);
    assert_eq!(operation_count(&operations, "code_mode.cell"), turns);
    assert_eq!(operation_count(&operations, "code_mode.host_spawn"), 1);
    assert_eq!(operation_count(&operations, "code_mode.cell_actor"), turns);
    assert_eq!(yielded_exec_spans, turns);
    assert_eq!(write_stdin_spans, turns);
    assert_eq!(
        operation_count(&operations, "mcp.tool_call"),
        turns * (parallel_calls + 1)
    );
    assert!(
        operation_count(&operations, "tool.execute") >= turns * (parallel_calls + 5),
        "too few nested tool spans: {}",
        operation_count(&operations, "tool.execute")
    );
    assert!(
        error_spans >= turns * 4,
        "expected at least four error spans per turn, saw {error_spans}"
    );
    Ok(())
}

fn validate_trace_content(traces: &[Value]) -> Result<()> {
    let encoded = serde_json::to_string(traces)?;
    assert!(!encoded.contains(PROMPT_SENTINEL));
    assert!(encoded.contains(REASONING_SENTINEL));
    assert!(!encoded.contains(REASONING_CONTENT_SENTINEL));
    assert!(!encoded.contains("turn-0-call-"));
    assert!(encoded.contains("assistant.output.bytes"));
    assert!(encoded.contains("model.input.bytes"));
    assert!(encoded.contains("model.output.bytes"));
    assert!(encoded.contains("tool.arguments.bytes"));
    assert!(!encoded.contains(API_KEY_SENTINEL));
    Ok(())
}

fn has_tag(span: &Value, key: &str, value: &str) -> bool {
    span["tags"].as_array().is_some_and(|tags| {
        tags.iter()
            .any(|tag| tag["key"] == key && tag["value"] == value)
    })
}

fn has_tag_key(span: &Value, key: &str) -> bool {
    span["tags"]
        .as_array()
        .is_some_and(|tags| tags.iter().any(|tag| tag["key"] == key))
}

fn operation_count(operations: &[String], operation: &str) -> usize {
    operations
        .iter()
        .filter(|name| name.as_str() == operation)
        .count()
}

fn event_count(events: &[Value], event_type: &str, tool: Option<&str>) -> usize {
    events
        .iter()
        .filter(|event| {
            event["type"] == event_type && tool.is_none_or(|tool| event["payload"]["tool"] == tool)
        })
        .count()
}

fn bounded_env(name: &str, default: usize, minimum: usize, maximum: usize) -> Result<usize> {
    let value = std::env::var(name).map_or(Ok(default), |value| {
        value
            .parse::<usize>()
            .wrap_err_with(|| format!("{name} must be an integer"))
    })?;
    if !(minimum..=maximum).contains(&value) {
        bail!("{name} must be in {minimum}..={maximum}");
    }
    Ok(value)
}

async fn next_json<S>(socket: &mut WebSocketStream<S>) -> Result<Value>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        let message = socket
            .next()
            .await
            .ok_or_else(|| eyre!("client closed before sending a request"))??;
        if let Message::Text(text) = message {
            return Ok(serde_json::from_str(text.as_str())?);
        }
    }
}

async fn send_completed<S>(
    socket: &mut WebSocketStream<S>,
    response_id: &str,
    output: &[Value],
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    socket
        .send(Message::Text(
            json!({
                "type": "response.completed",
                "response": {
                    "id": response_id,
                    "status": "completed",
                    "output": output,
                    "usage": {
                        "input_tokens": 10,
                        "input_tokens_details": { "cached_tokens": 5 },
                        "output_tokens": 2,
                        "output_tokens_details": { "reasoning_tokens": 1 },
                        "total_tokens": 12
                    }
                }
            })
            .to_string()
            .into(),
        ))
        .await?;
    Ok(())
}

fn temporary_workspace() -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "nanocodex-observability-stress-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}
