use std::{
    collections::{BTreeMap, HashMap},
    fs,
    hint::black_box,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use nanocodex_oai_api::{
    __private::EventSink,
    events::{AgentEvent, AgentEventKind, monotonic_now_ns},
    pricing::{ServiceTier, estimate},
    responses::{
        ContentItem, FunctionOutputBody, FunctionOutputContent, InputTokenDetails, MessageRole,
        ResponseItem, ServerEvent, Usage,
    },
    transport::EncodedRequest,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, value::RawValue};
use smallvec::SmallVec;
use tower::{
    ServiceBuilder, ServiceExt, limit::ConcurrencyLimitLayer, service_fn, timeout::TimeoutLayer,
};

#[derive(Clone)]
struct LargePrompt(Arc<str>);

#[derive(Serialize)]
struct ResponseCreate<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    model: &'static str,
    input: [InputMessage<'a>; 1],
    prompt_cache_key: &'static str,
    store: bool,
    stream: bool,
}

#[derive(Serialize)]
struct InputMessage<'a> {
    role: &'static str,
    content: &'a str,
}

#[derive(Serialize)]
struct CodeModeToolName {
    name: String,
    namespace: Option<String>,
}

#[derive(Serialize)]
struct CodeModeTurnMetadata<'a> {
    code_mode_tool_names: &'a BTreeMap<String, CodeModeToolName>,
}

#[derive(Deserialize)]
struct OutputTextDelta {
    #[serde(rename = "type")]
    _kind: String,
    delta: String,
}

#[derive(Deserialize)]
struct RetainedApiEvent {
    direction: String,
    event: Box<RawValue>,
}

#[derive(Serialize)]
struct ApiEventRef<'a> {
    direction: &'static str,
    transport: &'static str,
    phase: &'static str,
    model_call_index: u32,
    event: &'a RawValue,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum MetadataEvent {
    #[serde(rename = "response.metadata")]
    Metadata {
        #[serde(default)]
        headers: HashMap<String, String>,
    },
    #[serde(other)]
    Other,
}

#[derive(Serialize)]
struct ReplyPayload<'a> {
    message: &'a str,
}

#[derive(Serialize)]
struct LegacyValueAgentEvent {
    protocol_version: u32,
    request_id: &'static str,
    seq: u64,
    #[serde(rename = "type")]
    kind: &'static str,
    payload: Value,
}

#[derive(Serialize)]
struct RawAgentEvent {
    protocol_version: u32,
    request_id: &'static str,
    seq: u64,
    #[serde(rename = "type")]
    kind: &'static str,
    payload: Box<RawValue>,
}

#[derive(Deserialize, Serialize)]
struct ValueDecodedAgentEvent {
    protocol_version: u32,
    request_id: Arc<str>,
    seq: u64,
    #[serde(rename = "type")]
    kind: AgentEventKind,
    payload: Value,
}

#[derive(Clone, Deserialize, Serialize)]
struct HeapMessage {
    #[serde(rename = "type")]
    kind: String,
    role: MessageRole,
    content: Vec<ContentItem>,
}

#[derive(Clone, Deserialize, Serialize)]
struct InlineMessage {
    #[serde(rename = "type")]
    kind: String,
    role: MessageRole,
    content: SmallVec<[ContentItem; 1]>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StringContentItem {
    InputText { text: String },
    InputImage { image_url: String },
    OutputText { text: String },
}

#[derive(Clone, Deserialize, Serialize)]
struct StringInlineMessage {
    #[serde(rename = "type")]
    kind: String,
    role: MessageRole,
    content: SmallVec<[StringContentItem; 1]>,
}

const fn request(prompt: &str) -> ResponseCreate<'_> {
    ResponseCreate {
        kind: "response.create",
        model: "benchmark-model",
        input: [InputMessage {
            role: "user",
            content: prompt,
        }],
        prompt_cache_key: "stable-benchmark-session",
        store: false,
        stream: true,
    }
}

fn request_encoding(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("responses_request_encoding");
    group.sample_size(50);
    group.measurement_time(Duration::from_secs(3));
    for bytes in [16 * 1024, 128 * 1024, 1024 * 1024] {
        let prompt = "repository context and source code\n".repeat(bytes / 35 + 1);
        let prompt = &prompt[..bytes];
        group.throughput(Throughput::Bytes(bytes as u64));
        group.bench_with_input(
            BenchmarkId::new("serde_vec", bytes),
            &bytes,
            |bencher, _| {
                bencher.iter(|| serde_json::to_vec(black_box(&request(prompt))).unwrap());
            },
        );
        group.bench_with_input(
            BenchmarkId::new("encoded_raw_value", bytes),
            &bytes,
            |bencher, _| {
                bencher.iter(|| EncodedRequest::new(black_box(&request(prompt))).unwrap());
            },
        );
        group.bench_with_input(
            BenchmarkId::new("telemetry_input_bytes_then_encode", bytes),
            &bytes,
            |bencher, _| {
                bencher.iter(|| {
                    let request = request(prompt);
                    let input_bytes = serde_json::to_vec(black_box(&request.input)).unwrap().len();
                    let encoded = EncodedRequest::new(black_box(&request)).unwrap();
                    black_box((input_bytes, encoded))
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("two_telemetry_sizes_then_encode", bytes),
            &bytes,
            |bencher, _| {
                bencher.iter(|| {
                    let request = request(prompt);
                    let first_size = serde_json::to_vec(black_box(&request.input)).unwrap().len();
                    let second_size = serde_json::to_vec(black_box(&request.input)).unwrap().len();
                    let encoded = EncodedRequest::new(black_box(&request)).unwrap();
                    black_box((first_size, second_size, encoded))
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("encoded_send_ready", bytes),
            &bytes,
            |bencher, _| {
                bencher.iter(|| {
                    EncodedRequest::new(black_box(&request(prompt)))
                        .unwrap()
                        .into_string()
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("legacy_encoded_plus_send_copy", bytes),
            &bytes,
            |bencher, _| {
                bencher.iter(|| {
                    let encoded = EncodedRequest::new(black_box(&request(prompt))).unwrap();
                    encoded.raw().get().to_owned()
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("simd_json_raw_value", bytes),
            &bytes,
            |bencher, _| {
                bencher.iter(|| {
                    let json = simd_json::serde::to_string(black_box(&request(prompt))).unwrap();
                    RawValue::from_string(json).unwrap()
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("sonic_rs_raw_value", bytes),
            &bytes,
            |bencher, _| {
                bencher.iter(|| {
                    let json = sonic_rs::to_string(black_box(&request(prompt))).unwrap();
                    RawValue::from_string(json).unwrap()
                });
            },
        );
    }
    group.finish();
}

fn responses_lite_metadata_encoding(criterion: &mut Criterion) {
    let names = (0..64)
        .map(|index| {
            (
                format!("tool_{index:02}"),
                CodeModeToolName {
                    name: format!("tool_{index:02}"),
                    namespace: (index % 4 == 0).then(|| "mcp__fixture".to_owned()),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    criterion.bench_function(
        "responses_lite_metadata/code_mode_tool_names_64",
        |bencher| {
            bencher.iter(|| {
                serde_json::to_string(black_box(&CodeModeTurnMetadata {
                    code_mode_tool_names: &names,
                }))
                .unwrap()
            });
        },
    );
}

fn event_decoding(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("responses_event_decoding");
    group.sample_size(50);
    group.measurement_time(Duration::from_secs(3));
    // Local Codex history (2,074 final replies): p95=1,971 bytes,
    // p99=6,603 bytes, max=13,685 bytes. Round those up for stable fixtures.
    for bytes in [2 * 1024, 8 * 1024, 16 * 1024] {
        let delta = "model output ".repeat(bytes / 13 + 1);
        let encoded = serde_json::to_vec(&serde_json::json!({
            "type": "response.output_text.delta",
            "delta": &delta[..bytes],
        }))
        .unwrap();
        group.throughput(Throughput::Bytes(encoded.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("serde_json", bytes),
            &bytes,
            |bencher, _| {
                bencher.iter(|| {
                    let event: OutputTextDelta =
                        serde_json::from_slice(black_box(&encoded)).unwrap();
                    black_box(event.delta.len())
                });
            },
        );
        group.bench_with_input(BenchmarkId::new("sonic_rs", bytes), &bytes, |bencher, _| {
            bencher.iter(|| {
                let event: OutputTextDelta = sonic_rs::from_slice(black_box(&encoded)).unwrap();
                black_box(event.delta.len())
            });
        });
        group.bench_with_input(
            BenchmarkId::new("simd_json", bytes),
            &bytes,
            |bencher, _| {
                bencher.iter(|| {
                    let mut input = black_box(encoded.clone());
                    let event: OutputTextDelta = simd_json::serde::from_slice(&mut input).unwrap();
                    black_box(event.delta.len())
                });
            },
        );
    }
    group.finish();
}

fn agent_event_encoding(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("agent_final_event_encoding");
    let request_id: Arc<str> = Arc::from("realistic-reply-benchmark");
    group.sample_size(50);
    group.measurement_time(Duration::from_secs(3));
    for bytes in [2 * 1024, 8 * 1024, 16 * 1024] {
        let reply = "model output ".repeat(bytes / 13 + 1);
        let reply = &reply[..bytes];
        group.throughput(Throughput::Bytes(bytes as u64));
        group.bench_with_input(
            BenchmarkId::new("current_raw_payload", bytes),
            &bytes,
            |bencher, _| {
                bencher.iter(|| {
                    let payload = Arc::from(
                        serde_json::value::to_raw_value(&ReplyPayload {
                            message: black_box(reply),
                        })
                        .unwrap(),
                    );
                    serde_json::to_vec(&AgentEvent {
                        protocol_version: 1,
                        request_id: Arc::clone(&request_id),
                        seq: 1,
                        kind: AgentEventKind::AssistantMessage,
                        payload,
                    })
                    .unwrap()
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("minimal_raw_envelope", bytes),
            &bytes,
            |bencher, _| {
                bencher.iter(|| {
                    let payload = serde_json::value::to_raw_value(&ReplyPayload {
                        message: black_box(reply),
                    })
                    .unwrap();
                    serde_json::to_vec(&RawAgentEvent {
                        protocol_version: 1,
                        request_id: "realistic-reply-benchmark",
                        seq: 1,
                        kind: "assistant.message",
                        payload,
                    })
                    .unwrap()
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("legacy_value_payload", bytes),
            &bytes,
            |bencher, _| {
                bencher.iter(|| {
                    let payload = serde_json::to_value(ReplyPayload {
                        message: black_box(reply),
                    })
                    .unwrap();
                    serde_json::to_vec(&LegacyValueAgentEvent {
                        protocol_version: 1,
                        request_id: "realistic-reply-benchmark",
                        seq: 1,
                        kind: "assistant.message",
                        payload,
                    })
                    .unwrap()
                });
            },
        );
    }
    group.finish();
}

fn pricing_estimation(criterion: &mut Criterion) {
    let usage = Usage {
        input_tokens: 128_000,
        input_tokens_details: Some(InputTokenDetails {
            cached_tokens: 96_000,
            cache_write_tokens: 8_000,
        }),
        output_tokens: 16_000,
        output_tokens_details: None,
        total_tokens: 144_000,
    };
    let mut group = criterion.benchmark_group("pricing_estimation");
    group.sample_size(100);
    group.measurement_time(Duration::from_secs(3));
    group.bench_function("aggregate_turn_usage", |bencher| {
        bencher.iter(|| estimate(black_box(&usage), black_box(ServiceTier::Standard)));
    });
    group.finish();
}

fn timed_agent_event_delivery(criterion: &mut Criterion) {
    const EVENTS_PER_BATCH: u64 = 1_024;
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut group = criterion.benchmark_group("timed_agent_event_delivery");
    group.sample_size(50);
    group.measurement_time(Duration::from_secs(3));
    group.throughput(Throughput::Elements(EVENTS_PER_BATCH));
    group.bench_function("emit_receive_1024", |bencher| {
        bencher.to_async(&runtime).iter(|| async {
            let (sink, mut events) = EventSink::channel("benchmark-session".to_owned());
            for _ in 0..EVENTS_PER_BATCH {
                let source_received_ns = monotonic_now_ns();
                sink.emit_with_source_sequence(
                    AgentEventKind::AssistantDelta,
                    ReplyPayload { message: "delta" },
                    Some(source_received_ns),
                )
                .unwrap();
                let received = events.recv_timed().await.unwrap();
                black_box((
                    received.event.seq,
                    received.timing.emitted_ns,
                    received.timing.source_received_ns,
                ));
            }
        });
    });
    group.bench_function("emit_then_try_receive_1024", |bencher| {
        bencher.iter(|| {
            let (sink, mut events) = EventSink::channel("benchmark-session".to_owned());
            for _ in 0..EVENTS_PER_BATCH {
                sink.emit(
                    AgentEventKind::AssistantDelta,
                    ReplyPayload { message: "delta" },
                )
                .unwrap();
            }
            let received = std::iter::from_fn(|| events.try_recv_timed()).count();
            assert_eq!(
                u64::try_from(received).expect("event count should fit in u64"),
                EVENTS_PER_BATCH
            );
            black_box((sink, events));
        });
    });
    group.bench_function("emit_then_try_receive_mirrored_1024", |bencher| {
        bencher.iter(|| {
            let (sink, mut session_events) = EventSink::channel("benchmark-session".to_owned());
            let (turn_sink, mut turn_events) = sink.mirrored_channel();
            for _ in 0..EVENTS_PER_BATCH {
                turn_sink
                    .emit(
                        AgentEventKind::AssistantDelta,
                        ReplyPayload { message: "delta" },
                    )
                    .unwrap();
            }
            let session_received = std::iter::from_fn(|| session_events.try_recv_timed()).count();
            let turn_received = std::iter::from_fn(|| turn_events.try_recv_timed()).count();
            assert_eq!(
                (
                    u64::try_from(session_received).expect("event count should fit in u64"),
                    u64::try_from(turn_received).expect("event count should fit in u64"),
                ),
                (EVENTS_PER_BATCH, EVENTS_PER_BATCH)
            );
            black_box((sink, turn_sink, session_events, turn_events));
        });
    });
    let large_payload = "x".repeat(128 * 1_024);
    group.bench_function("emit_dropped_1024_large_payload", |bencher| {
        bencher.iter(|| {
            let (sink, events) = EventSink::channel("benchmark-session".to_owned());
            drop(events);
            for _ in 0..EVENTS_PER_BATCH {
                sink.emit(
                    AgentEventKind::ApiEvent,
                    ReplyPayload {
                        message: black_box(&large_payload),
                    },
                )
                .unwrap();
            }
            black_box(sink);
        });
    });
    group.finish();
}

fn decode_jsonl<T: DeserializeOwned>(encoded: &[u8]) -> Vec<T> {
    serde_json::Deserializer::from_slice(encoded)
        .into_iter::<T>()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn encode_jsonl<T: Serialize>(events: &[T]) -> Vec<u8> {
    let mut encoded = Vec::new();
    for event in events {
        serde_json::to_writer(&mut encoded, event).unwrap();
        encoded.push(b'\n');
    }
    encoded
}

fn retained_agent_event_trace(criterion: &mut Criterion) {
    let Some(path) = std::env::var_os("NANOCODEX_BENCH_EVENTS") else {
        eprintln!("NANOCODEX_BENCH_EVENTS is unset; skipping retained event-trace benchmarks");
        return;
    };
    let mut path = PathBuf::from(path);
    if path.is_relative() {
        path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(path);
    }
    let encoded =
        fs::read(&path).expect("NANOCODEX_BENCH_EVENTS should name a readable JSONL trace");
    let raw_events = decode_jsonl::<AgentEvent>(&encoded);
    let value_events = decode_jsonl::<ValueDecodedAgentEvent>(&encoded);
    assert_eq!(raw_events.len(), value_events.len());
    assert_eq!(encode_jsonl(&raw_events), encoded);
    for event in &raw_events {
        event.data().unwrap_or_else(|error| {
            panic!(
                "retained event {} ({:?}) violated its typed projection: {error}",
                event.seq, event.kind
            )
        });
    }
    black_box(value_events.iter().fold(0_u64, |checksum, event| {
        checksum
            ^ u64::from(event.protocol_version)
            ^ event.seq
            ^ event.request_id.len() as u64
            ^ event.kind as u64
            ^ event.payload.to_string().len() as u64
    }));

    eprintln!(
        "retained agent event trace: path={} events={} bytes={}",
        path.to_string_lossy(),
        raw_events.len(),
        encoded.len()
    );
    let mut group = criterion.benchmark_group("retained_agent_event_trace");
    group.sample_size(30);
    group.measurement_time(Duration::from_secs(5));
    group.throughput(Throughput::Bytes(encoded.len() as u64));
    group.bench_function("decode_raw_payload", |bencher| {
        bencher.iter(|| decode_jsonl::<AgentEvent>(black_box(&encoded)));
    });
    group.bench_function("decode_value_payload", |bencher| {
        bencher.iter(|| decode_jsonl::<ValueDecodedAgentEvent>(black_box(&encoded)));
    });
    group.bench_function("encode_raw_payload", |bencher| {
        bencher.iter(|| encode_jsonl(black_box(&raw_events)));
    });
    group.bench_function("encode_value_payload", |bencher| {
        bencher.iter(|| encode_jsonl(black_box(&value_events)));
    });
    group.bench_function("project_typed_domain_events", |bencher| {
        bencher.iter(|| {
            let projected = raw_events
                .iter()
                .filter_map(|event| event.data().ok())
                .count();
            black_box(projected)
        });
    });
    group.finish();
}

fn retained_response_event_pipeline(criterion: &mut Criterion) {
    let Some(path) = std::env::var_os("NANOCODEX_BENCH_EVENTS") else {
        return;
    };
    let mut path = PathBuf::from(path);
    if path.is_relative() {
        path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(path);
    }
    let events = retained_inbound_events(&path);
    let event_bytes = events.iter().map(|event| event.get().len()).sum::<usize>();
    eprintln!(
        "retained response events: events={} bytes={event_bytes}",
        events.len()
    );

    let mut group = criterion.benchmark_group("retained_response_event_pipeline");
    group.sample_size(30);
    group.measurement_time(Duration::from_secs(5));
    group.throughput(Throughput::Bytes(event_bytes as u64));
    group.bench_function("metadata_full_decode", |bencher| {
        bencher.iter(|| {
            let metadata_events = events
                .iter()
                .filter(|event| {
                    matches!(
                        serde_json::from_str::<MetadataEvent>(black_box(event.get())).unwrap(),
                        MetadataEvent::Metadata { headers } if !headers.is_empty()
                    )
                })
                .count();
            black_box(metadata_events)
        });
    });
    group.bench_function("metadata_type_prefix_guard", |bencher| {
        bencher.iter(|| {
            let metadata_events = events
                .iter()
                .filter(|event| {
                    (!event.get().starts_with(r#"{"type":""#)
                        || event.get().starts_with(r#"{"type":"response.metadata""#))
                        && matches!(
                            serde_json::from_str::<MetadataEvent>(event.get()).unwrap(),
                            MetadataEvent::Metadata { headers } if !headers.is_empty()
                        )
                })
                .count();
            black_box(metadata_events)
        });
    });
    group.bench_function("validate_raw", |bencher| {
        bencher.iter(|| {
            let bytes = events
                .iter()
                .map(|event| {
                    let decoded =
                        serde_json::from_str::<&RawValue>(black_box(event.get())).unwrap();
                    decoded.get().len()
                })
                .sum::<usize>();
            black_box(bytes)
        });
    });
    group.bench_function("decode_typed", |bencher| {
        bencher.iter(|| {
            let outputs = events
                .iter()
                .filter(|event| {
                    !matches!(
                        serde_json::from_str::<ServerEvent>(black_box(event.get())).unwrap(),
                        ServerEvent::Other
                    )
                })
                .count();
            black_box(outputs)
        });
    });
    group.bench_function("encode_event_payload", |bencher| {
        bencher.iter(|| {
            let bytes = events
                .iter()
                .map(|event| {
                    serde_json::value::to_raw_value(&ApiEventRef {
                        direction: "inbound",
                        transport: "responses_websocket_v2",
                        phase: "generation",
                        model_call_index: 1,
                        event,
                    })
                    .unwrap()
                    .get()
                    .len()
                })
                .sum::<usize>();
            black_box(bytes)
        });
    });
    group.finish();
}

fn retained_inbound_events(path: &Path) -> Vec<Box<RawValue>> {
    let encoded = fs::read(path).expect("NANOCODEX_BENCH_EVENTS should name a readable trace");
    decode_jsonl::<AgentEvent>(&encoded)
        .into_iter()
        .filter(|event| event.kind == AgentEventKind::ApiEvent)
        .filter_map(|event| serde_json::from_str::<RetainedApiEvent>(event.payload.get()).ok())
        .filter(|event| event.direction == "inbound")
        .map(|event| event.event)
        .collect()
}

fn realistic_history(bytes: usize) -> (Vec<u8>, Vec<Value>, Vec<ResponseItem>) {
    let encrypted = "opaque-reasoning-state".repeat(12);
    let tool_output = "bounded command output".repeat(16);
    let assistant = "concise assistant commentary".repeat(20);
    let mut values = Vec::with_capacity(256);
    while serde_json::to_vec(&values).unwrap().len() < bytes {
        let index = values.len();
        values.extend([
            serde_json::json!({
                "type": "reasoning",
                "id": format!("rs-{index}"),
                "summary": [],
                "encrypted_content": encrypted,
                "internal_chat_message_metadata_passthrough": {"turn_id": "turn-benchmark"}
            }),
            serde_json::json!({
                "type": "custom_tool_call",
                "id": format!("ctc-{index}"),
                "status": "completed",
                "call_id": format!("call-{index}"),
                "name": "exec",
                "input": "text(await tools.exec_command({cmd: \"rg --files\"}))",
                "internal_chat_message_metadata_passthrough": {"turn_id": "turn-benchmark"}
            }),
            serde_json::json!({
                "type": "custom_tool_call_output",
                "call_id": format!("call-{index}"),
                "output": [
                    {"type": "input_text", "text": tool_output},
                    {"type": "input_text", "text": "exit_code=0"}
                ],
                "internal_chat_message_metadata_passthrough": {"turn_id": "turn-benchmark"}
            }),
            serde_json::json!({
                "type": "message",
                "id": format!("msg-{index}"),
                "role": "assistant",
                "status": "completed",
                "content": [{
                    "type": "output_text",
                    "text": assistant,
                    "annotations": [],
                    "logprobs": []
                }],
                "phase": "commentary",
                "internal_chat_message_metadata_passthrough": {"turn_id": "turn-benchmark"}
            }),
        ]);
    }
    let encoded = serde_json::to_vec(&values).unwrap();
    let typed = serde_json::from_slice(&encoded).unwrap();
    (encoded, values, typed)
}

fn response_item_history(criterion: &mut Criterion) {
    let (encoded, values, typed) = realistic_history(128 * 1024);
    let typed_arc = Arc::new(typed.clone());
    let mut group = criterion.benchmark_group("response_item_history_128k");
    group.sample_size(50);
    group.measurement_time(Duration::from_secs(3));
    group.throughput(Throughput::Bytes(encoded.len() as u64));

    group.bench_function("decode_value", |bencher| {
        bencher.iter(|| serde_json::from_slice::<Vec<Value>>(black_box(&encoded)).unwrap());
    });
    group.bench_function("decode_typed", |bencher| {
        bencher.iter(|| serde_json::from_slice::<Vec<ResponseItem>>(black_box(&encoded)).unwrap());
    });
    group.bench_function("encode_value", |bencher| {
        bencher.iter(|| serde_json::to_vec(black_box(&values)).unwrap());
    });
    group.bench_function("encode_typed", |bencher| {
        bencher.iter(|| serde_json::to_vec(black_box(&typed)).unwrap());
    });
    group.bench_function("deep_clone_value", |bencher| {
        bencher.iter(|| black_box(values.clone()));
    });
    group.bench_function("deep_clone_typed", |bencher| {
        bencher.iter(|| black_box(typed.clone()));
    });
    group.bench_function("attempt_arc_clone", |bencher| {
        bencher.iter(|| Arc::clone(black_box(&typed_arc)));
    });
    group.finish();
}

fn message_content_storage(criterion: &mut Criterion) {
    let fixture = serde_json::to_vec(&serde_json::json!({
        "type": "message",
        "role": "assistant",
        "status": "completed",
        "content": [{
            "type": "output_text",
            "text": "model output ".repeat(512),
            "annotations": [],
            "logprobs": []
        }]
    }))
    .unwrap();
    let heap: HeapMessage = serde_json::from_slice(&fixture).unwrap();
    let inline: InlineMessage = serde_json::from_slice(&fixture).unwrap();
    let string_inline: StringInlineMessage = serde_json::from_slice(&fixture).unwrap();
    eprintln!(
        "message layout bytes: response_item={} function_output_body={} function_output_content={} vec_box={} smallvec_box={} smallvec_string={} content_box={} content_string={}",
        std::mem::size_of::<ResponseItem>(),
        std::mem::size_of::<FunctionOutputBody>(),
        std::mem::size_of::<FunctionOutputContent>(),
        std::mem::size_of::<HeapMessage>(),
        std::mem::size_of::<InlineMessage>(),
        std::mem::size_of::<StringInlineMessage>(),
        std::mem::size_of::<ContentItem>(),
        std::mem::size_of::<StringContentItem>(),
    );
    let mut group = criterion.benchmark_group("message_content_one_item");
    group.sample_size(50);
    group.measurement_time(Duration::from_secs(3));
    group.throughput(Throughput::Bytes(fixture.len() as u64));

    group.bench_function("decode_vec", |bencher| {
        bencher.iter(|| serde_json::from_slice::<HeapMessage>(black_box(&fixture)).unwrap());
    });
    group.bench_function("decode_smallvec_1", |bencher| {
        bencher.iter(|| serde_json::from_slice::<InlineMessage>(black_box(&fixture)).unwrap());
    });
    group.bench_function("decode_smallvec_1_string", |bencher| {
        bencher
            .iter(|| serde_json::from_slice::<StringInlineMessage>(black_box(&fixture)).unwrap());
    });
    group.bench_function("clone_vec", |bencher| {
        bencher.iter(|| black_box(heap.clone()));
    });
    group.bench_function("clone_smallvec_1", |bencher| {
        bencher.iter(|| black_box(inline.clone()));
    });
    group.bench_function("clone_smallvec_1_string", |bencher| {
        bencher.iter(|| black_box(string_inline.clone()));
    });
    group.finish();
}

fn tower_dispatch(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let prompt = LargePrompt(Arc::from("large prompt token ".repeat(8_192)));
    let mut group = criterion.benchmark_group("responses_dispatch_128k_prompt");
    group.sample_size(50);
    group.measurement_time(Duration::from_secs(3));

    group.bench_function("direct_async", |bencher| {
        bencher
            .to_async(&runtime)
            .iter(|| async { black_box(black_box(prompt.clone()).0.len()) });
    });

    group.bench_function("tower_service", |bencher| {
        let service = service_fn(|request: LargePrompt| async move {
            Ok::<_, tower::BoxError>(black_box(request.0.len()))
        });
        bencher.to_async(&runtime).iter(|| {
            let prompt = prompt.clone();
            service.oneshot(black_box(prompt))
        });
    });

    group.bench_function("tower_limit_timeout_stack", |bencher| {
        let service = ServiceBuilder::new()
            .layer(ConcurrencyLimitLayer::new(1))
            .layer(TimeoutLayer::new(Duration::from_secs(30)))
            .service(service_fn(|request: LargePrompt| async move {
                Ok::<_, tower::BoxError>(black_box(request.0.len()))
            }));
        bencher.to_async(&runtime).iter(|| {
            let service = service.clone();
            let prompt = prompt.clone();
            async move { black_box(service.oneshot(black_box(prompt)).await.unwrap()) }
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    request_encoding,
    responses_lite_metadata_encoding,
    event_decoding,
    agent_event_encoding,
    pricing_estimation,
    timed_agent_event_delivery,
    retained_agent_event_trace,
    retained_response_event_pipeline,
    response_item_history,
    message_content_storage,
    tower_dispatch
);
criterion_main!(benches);
