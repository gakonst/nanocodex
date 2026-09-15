use std::{
    collections::{BTreeMap, HashMap},
    io::{self, Write},
    path::Path,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use chromiumoxide::{
    Page,
    cdp::browser_protocol::tracing::{
        EndParams, EventDataCollected, EventTracingComplete, StartParams, StartTransferMode,
        TraceConfig,
    },
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{sync::oneshot, task::JoinHandle, time::timeout};

use crate::{
    BrowserNetworkRequest, BrowserPerformanceInsight, BrowserPerformanceSource,
    BrowserPerformanceTrace, trace_serialized,
};

use super::{BrowserError, evaluate_typed, source_maps::SourceMaps};

const MAX_TRACE_EVENTS: usize = 250_000;
const MAX_TRACE_BYTES: u64 = 128 * 1024 * 1024;
const TRACE_STOP_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) struct PerformanceTraceState {
    page: Page,
    events: Arc<StdMutex<Vec<Value>>>,
    dropped: Arc<AtomicU64>,
    complete: oneshot::Receiver<bool>,
    task: Option<JoinHandle<()>>,
    request_after: u64,
    ended: bool,
}

impl Drop for PerformanceTraceState {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
        if self.ended {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let page = self.page.clone();
        runtime.spawn(async move {
            let _ = page.execute(EndParams::default()).await;
        });
    }
}

pub(super) async fn start(
    page: &Page,
    request_after: u64,
) -> Result<PerformanceTraceState, BrowserError> {
    let mut data = page.event_listener::<EventDataCollected>().await?;
    let mut completed = page.event_listener::<EventTracingComplete>().await?;
    let events = Arc::new(StdMutex::new(Vec::new()));
    let task_events = Arc::clone(&events);
    let dropped = Arc::new(AtomicU64::new(0));
    let task_dropped = Arc::clone(&dropped);
    let retained_bytes = Arc::new(AtomicU64::new(0));
    let task_retained_bytes = Arc::clone(&retained_bytes);
    let (complete_tx, complete) = oneshot::channel();
    let task = tokio::spawn(async move {
        let data_loss = loop {
            tokio::select! {
                event = data.next() => {
                    let Some(event) = event else {
                        break true;
                    };
                    trace_serialized("devtools.Tracing.dataCollected", event.as_ref());
                    if !retain_events(
                        &task_events,
                        &task_dropped,
                        &task_retained_bytes,
                        &event.value,
                    ) {
                        break true;
                    }
                }
                event = completed.next() => {
                    if let Some(event) = event.as_ref() {
                        trace_serialized("devtools.Tracing.tracingComplete", event.as_ref());
                    }
                    let data_loss = event.is_none_or(|event| event.data_loss_occurred);
                    while let Ok(Some(event)) =
                        timeout(Duration::from_millis(50), data.next()).await
                    {
                        trace_serialized("devtools.Tracing.dataCollected", event.as_ref());
                        if !retain_events(
                            &task_events,
                            &task_dropped,
                            &task_retained_bytes,
                            &event.value,
                        ) {
                            break;
                        }
                    }
                    break data_loss;
                }
            }
        };
        let _ = complete_tx.send(data_loss);
    });

    let config = TraceConfig::builder()
        .included_categories([
            "blink",
            "devtools.timeline",
            "disabled-by-default-devtools.timeline",
            "disabled-by-default-devtools.timeline.frame",
            "disabled-by-default-devtools.timeline.invalidationTracking",
            "disabled-by-default-devtools.timeline.stack",
            "disabled-by-default-v8.cpu_profiler",
            "disabled-by-default-v8.cpu_profiler.hires",
            "blink.user_timing",
            "devtools.timeline.frame",
            "latencyInfo",
            "loading",
            "navigation",
            "toplevel",
            "v8",
        ])
        .build();
    page.execute(
        StartParams::builder()
            .transfer_mode(StartTransferMode::ReportEvents)
            .trace_config(config)
            .build(),
    )
    .await?;
    Ok(PerformanceTraceState {
        page: page.clone(),
        events,
        dropped,
        complete,
        task: Some(task),
        request_after,
        ended: false,
    })
}

fn retain_events(
    events: &StdMutex<Vec<Value>>,
    dropped: &AtomicU64,
    retained_bytes: &AtomicU64,
    incoming: &[Value],
) -> bool {
    retain_events_with_limits(
        events,
        dropped,
        retained_bytes,
        incoming,
        MAX_TRACE_EVENTS,
        MAX_TRACE_BYTES,
    )
}

fn retain_events_with_limits(
    events: &StdMutex<Vec<Value>>,
    dropped: &AtomicU64,
    retained_bytes: &AtomicU64,
    incoming: &[Value],
    max_events: usize,
    max_bytes: u64,
) -> bool {
    let Ok(mut retained) = events.lock() else {
        return false;
    };
    let mut bytes = retained_bytes.load(Ordering::Relaxed);
    for event in incoming {
        let mut counter = ByteCounter::default();
        let event_bytes =
            serde_json::to_writer(&mut counter, event).map_or(u64::MAX, |()| counter.bytes);
        if retained.len() == max_events || bytes.saturating_add(event_bytes) > max_bytes {
            dropped.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        bytes = bytes.saturating_add(event_bytes);
        retained.push(event.clone());
    }
    retained_bytes.store(bytes, Ordering::Relaxed);
    true
}

#[derive(Default)]
struct ByteCounter {
    bytes: u64,
}

impl Write for ByteCounter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .saturating_add(u64::try_from(buffer.len()).unwrap_or(u64::MAX));
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        path::PathBuf,
        sync::{
            Mutex,
            atomic::{AtomicU64, Ordering},
        },
    };

    use serde_json::json;

    use crate::BrowserPerformanceInsight;

    use super::{PerformanceContext, retain_events_with_limits, summarize};

    #[test]
    fn trace_retention_enforces_count_and_encoded_byte_budgets() {
        let retained = Mutex::new(Vec::new());
        let dropped = AtomicU64::new(0);
        let bytes = AtomicU64::new(0);
        let incoming = [json!({"name": "first"}), json!({"name": "second"})];

        assert!(retain_events_with_limits(
            &retained, &dropped, &bytes, &incoming, 10, 17,
        ));
        assert_eq!(retained.lock().expect("retained trace").len(), 1);
        assert_eq!(dropped.load(Ordering::Relaxed), 1);

        assert!(retain_events_with_limits(
            &retained,
            &dropped,
            &bytes,
            &[json!({"name": "third"})],
            1,
            u64::MAX,
        ));
        assert_eq!(retained.lock().expect("retained trace").len(), 1);
        assert_eq!(dropped.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn trace_bounds_ignore_metadata_without_timestamps() {
        let absolute_start = 159_432_556_000.0;
        let events = vec![
            json!({"name": "thread_name", "ph": "M"}),
            json!({"name": "process_name", "ph": "M", "ts": null}),
            json!({
                "name": "RunTask",
                "ts": absolute_start,
                "dur": 7_000_000.0
            }),
        ];

        let trace = summarize(&events, PathBuf::from("trace.json"), &[], &empty_context());

        assert_eq!(trace.event_count, 3);
        assert_eq!(trace.duration_ms, 7_000.0);
        assert_eq!(trace.long_task_count, 1);
        assert_eq!(trace.longest_task_ms, 7_000.0);
        assert!(trace.insights.iter().any(|insight| matches!(
            insight,
            BrowserPerformanceInsight::LongTask { start_ms, duration_ms, .. }
                if *start_ms == 0.0 && *duration_ms == 7_000.0
        )));
    }

    #[test]
    fn trace_bounds_ignore_zero_timestamp_metadata_from_another_clock_domain() {
        let absolute_start = 163_962_202_000.0;
        let events = vec![
            json!({"name": "thread_name", "ph": "M", "ts": 0.0}),
            json!({"name": "navigationStart", "ts": absolute_start}),
            json!({
                "name": "RunTask",
                "ts": absolute_start + 1_000_000.0,
                "dur": 120_305.0
            }),
            json!({"name": "trace_end", "ts": absolute_start + 2_030_000.0}),
        ];

        let trace = summarize(&events, PathBuf::from("trace.json"), &[], &empty_context());

        assert_eq!(trace.duration_ms, 2_030.0);
        assert_eq!(trace.longest_task_ms, 120.305);
        assert!(trace.insights.iter().any(|insight| matches!(
            insight,
            BrowserPerformanceInsight::LongTask { start_ms, duration_ms, .. }
                if *start_ms == 1_000.0 && *duration_ms == 120.305
        )));
    }

    #[test]
    fn time_based_insights_are_relative_to_large_trace_timestamps() {
        let absolute_start = 159_432_556_000.0;
        let events = vec![
            json!({"name": "navigationStart", "ts": absolute_start}),
            json!({
                "name": "Layout",
                "ts": absolute_start + 1_000.0,
                "dur": 2_000.0,
                "args": {
                    "beginData": {
                        "stackTrace": [{
                            "scriptId": "7",
                            "functionName": "layout",
                            "url": "https://example.test/app.js",
                            "lineNumber": 0,
                            "columnNumber": 1
                        }]
                    }
                }
            }),
            json!({
                "name": "LayoutShift",
                "ts": absolute_start + 2_000.0,
                "args": {"data": {"score": 0.01, "hadRecentInput": false}}
            }),
            json!({
                "name": "largestContentfulPaint::Candidate",
                "ts": absolute_start + 3_000.0,
                "args": {"data": {"size": 42.0, "nodeName": "IMG"}}
            }),
            json!({
                "name": "RunTask",
                "ts": absolute_start + 4_000.0,
                "dur": 60_000.0
            }),
        ];

        let trace = summarize(&events, PathBuf::from("trace.json"), &[], &empty_context());

        assert_eq!(trace.duration_ms, 64.0);
        assert_eq!(trace.rendering_ms, 2.0);
        assert!(trace.insights.iter().any(|insight| matches!(
            insight,
            BrowserPerformanceInsight::ForcedReflow { start_ms, .. } if *start_ms == 1.0
        )));
        assert!(trace.insights.iter().any(|insight| matches!(
            insight,
            BrowserPerformanceInsight::LayoutShift { start_ms, .. } if *start_ms == 2.0
        )));
        assert!(trace.insights.iter().any(|insight| matches!(
            insight,
            BrowserPerformanceInsight::LargestContentfulPaint { start_ms, .. }
                if *start_ms == 3.0
        )));
        assert!(trace.insights.iter().any(|insight| matches!(
            insight,
            BrowserPerformanceInsight::LongTask { start_ms, .. } if *start_ms == 4.0
        )));
    }

    fn empty_context() -> PerformanceContext {
        PerformanceContext {
            origin: "https://example.test".to_owned(),
            nodes: 1,
            maximum_depth: 1,
            maximum_children: 0,
            blocking_urls: BTreeSet::new(),
        }
    }
}

pub(super) async fn stop(
    page: &Page,
    output_dir: &Path,
    sequence: u64,
    mut state: PerformanceTraceState,
    requests: &[BrowserNetworkRequest],
    source_maps: &SourceMaps,
) -> Result<BrowserPerformanceTrace, BrowserError> {
    page.execute(EndParams::default()).await?;
    state.ended = true;
    let data_loss = timeout(TRACE_STOP_TIMEOUT, &mut state.complete)
        .await
        .map_err(|_| BrowserError::PerformanceTraceTimeout)?
        .map_err(|_| BrowserError::PerformanceTraceUnavailable)?;
    if let Some(task) = state.task.take() {
        task.abort();
        let _ = task.await;
    }
    let events = state
        .events
        .lock()
        .map_err(|_| BrowserError::DiagnosticsUnavailable)?
        .clone();
    let dropped = state.dropped.load(Ordering::Relaxed);
    if data_loss || dropped > 0 {
        return Err(BrowserError::PerformanceTraceDataLoss { dropped });
    }

    let path = output_dir.join(format!("performance-trace-{sequence}.json"));
    let encoded = serde_json::to_vec(&TraceArtifact {
        trace_events: &events,
    })?;
    tokio::fs::write(&path, encoded).await?;
    let context =
        evaluate_typed::<PerformanceContext>(page, format!("({PERFORMANCE_CONTEXT_SCRIPT})()"))
            .await?;
    let requests = requests
        .iter()
        .filter(|request| request.sequence > state.request_after)
        .cloned()
        .collect::<Vec<_>>();
    let mut trace = summarize(&events, path, &requests, &context);
    source_maps.symbolicate_performance(&mut trace.insights)?;
    Ok(trace)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TraceArtifact<'a> {
    trace_events: &'a [Value],
}

#[allow(
    clippy::too_many_lines,
    reason = "one pass preserves exact trace ordering while deriving all bounded insight classes"
)]
fn summarize(
    events: &[Value],
    path: std::path::PathBuf,
    requests: &[BrowserNetworkRequest],
    context: &PerformanceContext,
) -> BrowserPerformanceTrace {
    let trace_bounds = trace_time_bounds(events);
    let first_timestamp = trace_bounds.map(|(first, _)| first);
    let mut scripting = 0.0_f64;
    let mut rendering = 0.0_f64;
    let mut painting = 0.0_f64;
    let mut long_task_count = 0_usize;
    let mut longest_task = 0.0_f64;
    let mut insights = Vec::new();

    for event in events {
        let Some(object) = event.as_object() else {
            continue;
        };
        let timestamp = event_timestamp(object);
        let duration = event_duration(object);
        let duration_ms = duration / 1_000.0;
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if is_scripting(name) {
            scripting += duration_ms;
        } else if is_rendering(name) {
            rendering += duration_ms;
        } else if is_painting(name) {
            painting += duration_ms;
        }
        if name == "RunTask" && duration_ms > 50.0 {
            long_task_count = long_task_count.saturating_add(1);
            longest_task = longest_task.max(duration_ms);
            if let Some(start_ms) = relative_start_ms(timestamp, first_timestamp) {
                insights.push(BrowserPerformanceInsight::LongTask {
                    start_ms,
                    duration_ms,
                    source: trace_source(object),
                });
            }
        }
        if name == "Layout"
            && duration_ms > 0.0
            && let Some(source) = trace_source(object)
            && let Some(start_ms) = relative_start_ms(timestamp, first_timestamp)
        {
            insights.push(BrowserPerformanceInsight::ForcedReflow {
                start_ms,
                duration_ms,
                source: Some(source),
            });
        }
        if name == "LayoutShift"
            && let Some(data) = object_path(object, &["args", "data"]).and_then(Value::as_object)
            && let Some(start_ms) = relative_start_ms(timestamp, first_timestamp)
        {
            insights.push(BrowserPerformanceInsight::LayoutShift {
                start_ms,
                score: data.get("score").and_then(Value::as_f64).unwrap_or(0.0),
                had_recent_input: data
                    .get("had_recent_input")
                    .or_else(|| data.get("hadRecentInput"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            });
        }
        if name.to_ascii_lowercase().contains("largestcontentfulpaint")
            && name.to_ascii_lowercase().contains("candidate")
            && let Some(data) = object_path(object, &["args", "data"]).and_then(Value::as_object)
            && let Some(start_ms) = relative_start_ms(timestamp, first_timestamp)
        {
            insights.push(BrowserPerformanceInsight::LargestContentfulPaint {
                start_ms,
                size: data.get("size").and_then(Value::as_f64),
                element: data
                    .get("nodeName")
                    .or_else(|| data.get("element"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                url: data.get("url").and_then(Value::as_str).map(str::to_owned),
            });
        }
        collect_selector_insights(object, &mut insights);
    }
    collect_network_insights(requests, context, &mut insights);
    insights.push(BrowserPerformanceInsight::DomSize {
        nodes: context.nodes,
        maximum_depth: context.maximum_depth,
        maximum_children: context.maximum_children,
    });
    insights.sort_by(|left, right| insight_weight(right).total_cmp(&insight_weight(left)));
    insights.truncate(100);

    let duration_ms = trace_bounds.map_or(0.0, |(first, final_timestamp)| {
        (final_timestamp - first).max(0.0) / 1_000.0
    });
    BrowserPerformanceTrace {
        path,
        event_count: events.len(),
        duration_ms,
        scripting_ms: scripting,
        rendering_ms: rendering,
        painting_ms: painting,
        long_task_count,
        longest_task_ms: longest_task,
        insights,
    }
}

fn trace_time_bounds(events: &[Value]) -> Option<(f64, f64)> {
    let mut bounds: Option<(f64, f64)> = None;
    for object in events.iter().filter_map(Value::as_object) {
        if object.get("ph").and_then(Value::as_str) == Some("M") {
            continue;
        }
        let Some(timestamp) = event_timestamp(object) else {
            continue;
        };
        let end = timestamp + event_duration(object);
        let end = if end.is_finite() { end } else { timestamp };
        bounds = Some(match bounds {
            Some((first, final_timestamp)) => (first.min(timestamp), final_timestamp.max(end)),
            None => (timestamp, end),
        });
    }
    bounds
}

fn event_timestamp(object: &serde_json::Map<String, Value>) -> Option<f64> {
    object
        .get("ts")
        .and_then(Value::as_f64)
        .filter(|timestamp| timestamp.is_finite())
}

fn event_duration(object: &serde_json::Map<String, Value>) -> f64 {
    object
        .get("dur")
        .and_then(Value::as_f64)
        .filter(|duration| duration.is_finite())
        .unwrap_or(0.0)
}

fn relative_start_ms(timestamp: Option<f64>, first_timestamp: Option<f64>) -> Option<f64> {
    let start_ms = (timestamp? - first_timestamp?) / 1_000.0;
    start_ms.is_finite().then_some(start_ms)
}

fn collect_selector_insights(
    event: &serde_json::Map<String, Value>,
    insights: &mut Vec<BrowserPerformanceInsight>,
) {
    let Some(stats) = object_path(event, &["args", "data", "selectorStats"])
        .or_else(|| object_path(event, &["args", "selectorStats"]))
        .and_then(Value::as_array)
    else {
        return;
    };
    for stat in stats {
        let Some(stat) = stat.as_object() else {
            continue;
        };
        let selector = stat
            .get("selector")
            .or_else(|| stat.get("selectorText"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let duration_ms = stat
            .get("elapsed")
            .or_else(|| stat.get("elapsedUs"))
            .and_then(Value::as_f64)
            .unwrap_or_default()
            / 1_000.0;
        if selector.is_empty() || duration_ms < 1.0 {
            continue;
        }
        insights.push(BrowserPerformanceInsight::SlowCssSelector {
            selector: selector.to_owned(),
            duration_ms,
            match_attempts: stat.get("matchAttempts").and_then(Value::as_u64),
        });
    }
}

fn collect_network_insights(
    requests: &[BrowserNetworkRequest],
    context: &PerformanceContext,
    insights: &mut Vec<BrowserPerformanceInsight>,
) {
    for request in requests {
        if context.blocking_urls.contains(&request.url) {
            insights.push(BrowserPerformanceInsight::RenderBlockingResource {
                url: request.url.clone(),
                resource_type: request.resource_type.clone(),
                duration_ms: request.duration_ms,
                encoded_bytes: request.encoded_data_length,
            });
        }
        if cache_opportunity(request) {
            insights.push(BrowserPerformanceInsight::CacheOpportunity {
                url: request.url.clone(),
                transferred_bytes: request.encoded_data_length.unwrap_or_default(),
            });
        }
    }

    let requests_by_id = requests
        .iter()
        .map(|request| (request.request_id.as_str(), request))
        .collect::<HashMap<_, _>>();
    for request in requests {
        let mut chain = vec![request.url.clone()];
        let mut duration_ms = request.duration_ms.unwrap_or_default();
        let mut parent_id = request
            .initiator
            .as_ref()
            .and_then(|initiator| initiator.request_id.as_deref());
        while let Some(parent) = parent_id {
            let Some(parent) = requests_by_id.get(parent) else {
                break;
            };
            chain.push(parent.url.clone());
            duration_ms = duration_ms.saturating_add(parent.duration_ms.unwrap_or_default());
            parent_id = parent
                .initiator
                .as_ref()
                .and_then(|initiator| initiator.request_id.as_deref());
            if chain.len() == 10 {
                break;
            }
        }
        if chain.len() > 1 {
            chain.reverse();
            insights.push(BrowserPerformanceInsight::NetworkDependencyChain {
                urls: chain,
                duration_ms,
            });
        }
    }

    let mut scripts = BTreeMap::<&str, Vec<&BrowserNetworkRequest>>::new();
    for request in requests
        .iter()
        .filter(|request| request.resource_type.eq_ignore_ascii_case("script"))
    {
        scripts.entry(&request.url).or_default().push(request);
    }
    for (url, requests) in scripts {
        if requests.len() > 1 {
            insights.push(BrowserPerformanceInsight::DuplicateJavaScript {
                url: url.to_owned(),
                request_count: requests.len(),
                transferred_bytes: requests
                    .iter()
                    .filter_map(|request| request.encoded_data_length)
                    .sum(),
            });
        }
    }

    let mut third_parties = BTreeMap::<String, (usize, u64)>::new();
    for request in requests {
        let Ok(url) = url::Url::parse(&request.url) else {
            continue;
        };
        let origin = url.origin().ascii_serialization();
        if origin == context.origin {
            continue;
        }
        let entry = third_parties.entry(origin).or_default();
        entry.0 = entry.0.saturating_add(1);
        entry.1 = entry
            .1
            .saturating_add(request.encoded_data_length.unwrap_or_default());
    }
    for (origin, (request_count, transferred_bytes)) in third_parties {
        insights.push(BrowserPerformanceInsight::ThirdParty {
            origin,
            request_count,
            transferred_bytes,
        });
    }
}

fn cache_opportunity(request: &BrowserNetworkRequest) -> bool {
    if request.method != "GET"
        || request.from_disk_cache
        || request.from_service_worker
        || request.encoded_data_length.unwrap_or_default() < 100_000
    {
        return false;
    }
    !request.response_headers.iter().any(|header| {
        header.name.eq_ignore_ascii_case("cache-control")
            && header.value.as_deref().is_some_and(|value| {
                value.contains("max-age") || value.contains("immutable") || value.contains("public")
            })
    })
}

fn trace_source(event: &serde_json::Map<String, Value>) -> Option<BrowserPerformanceSource> {
    let stack = object_path(event, &["args", "beginData", "stackTrace"])
        .or_else(|| object_path(event, &["args", "data", "stackTrace"]))
        .or_else(|| object_path(event, &["args", "stackTrace"]))
        .and_then(Value::as_array)?;
    let frame = stack.first()?.as_object()?;
    Some(BrowserPerformanceSource {
        script_id: frame
            .get("scriptId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        function_name: frame
            .get("functionName")
            .and_then(Value::as_str)
            .map(str::to_owned),
        url: frame
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        line_number: frame
            .get("lineNumber")
            .and_then(Value::as_u64)
            .map(|line| line.saturating_add(1)),
        column_number: frame
            .get("columnNumber")
            .and_then(Value::as_u64)
            .map(|column| column.saturating_add(1)),
        original: None,
    })
}

fn object_path<'a>(object: &'a serde_json::Map<String, Value>, path: &[&str]) -> Option<&'a Value> {
    let (first, rest) = path.split_first()?;
    let mut value = object.get(*first)?;
    for key in rest {
        value = value.as_object()?.get(*key)?;
    }
    Some(value)
}

#[allow(
    clippy::cast_precision_loss,
    reason = "ranking bounded diagnostics does not require exact integer representation above 2^53"
)]
fn insight_weight(insight: &BrowserPerformanceInsight) -> f64 {
    match insight {
        BrowserPerformanceInsight::LongTask { duration_ms, .. }
        | BrowserPerformanceInsight::ForcedReflow { duration_ms, .. }
        | BrowserPerformanceInsight::SlowCssSelector { duration_ms, .. } => *duration_ms,
        BrowserPerformanceInsight::LayoutShift { score, .. } => score * 1_000.0,
        BrowserPerformanceInsight::LargestContentfulPaint { start_ms, .. } => *start_ms,
        BrowserPerformanceInsight::RenderBlockingResource { duration_ms, .. } => {
            duration_ms.unwrap_or_default() as f64
        }
        BrowserPerformanceInsight::NetworkDependencyChain { duration_ms, .. } => {
            *duration_ms as f64
        }
        BrowserPerformanceInsight::DuplicateJavaScript {
            transferred_bytes, ..
        }
        | BrowserPerformanceInsight::CacheOpportunity {
            transferred_bytes, ..
        }
        | BrowserPerformanceInsight::ThirdParty {
            transferred_bytes, ..
        } => *transferred_bytes as f64 / 1_000.0,
        BrowserPerformanceInsight::DomSize { nodes, .. } => *nodes as f64 / 10.0,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PerformanceContext {
    origin: String,
    nodes: usize,
    maximum_depth: usize,
    maximum_children: usize,
    blocking_urls: std::collections::BTreeSet<String>,
}

const PERFORMANCE_CONTEXT_SCRIPT: &str = r#"function() {
  let nodes = 0;
  let maximumDepth = 0;
  let maximumChildren = 0;
  const visit = (node, depth) => {
    nodes++;
    maximumDepth = Math.max(maximumDepth, depth);
    maximumChildren = Math.max(maximumChildren, node.children?.length || 0);
    for (const child of node.children || []) visit(child, depth + 1);
    if (node.shadowRoot) visit(node.shadowRoot, depth + 1);
  };
  visit(document.documentElement, 1);
  const blockingUrls = [
    ...document.querySelectorAll(
      'link[rel~="stylesheet"][href]:not([media="print"]),script[src]:not([async]):not([defer]):not([type="module"])'
    )
  ].map(element => element.href || element.src).filter(Boolean);
  return {
    origin: location.origin,
    nodes,
    maximumDepth,
    maximumChildren,
    blockingUrls
  };
}"#;

fn is_scripting(name: &str) -> bool {
    matches!(
        name,
        "EvaluateScript" | "EventDispatch" | "FunctionCall" | "TimerFire" | "V8.Execute" | "v8.run"
    )
}

fn is_rendering(name: &str) -> bool {
    matches!(
        name,
        "Layout" | "RecalculateStyles" | "UpdateLayoutTree" | "HitTest"
    )
}

fn is_painting(name: &str) -> bool {
    matches!(name, "Paint" | "CompositeLayers" | "Commit" | "RasterTask")
}
