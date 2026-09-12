//! Source-pinned callback schedules, driven through the actual FIFO owner.
use super::*;
use std::{sync::mpsc, thread};

#[derive(Default)]
struct Progress {
    started: Vec<String>,
    operations: Vec<Value>,
}
#[derive(Default)]
struct Harness {
    state: State,
    progress: Mutex<Progress>,
    releases: Mutex<BTreeMap<String, mpsc::Sender<bool>>>,
    threads: Mutex<Vec<thread::JoinHandle<()>>>,
}
impl Harness {
    fn enqueue(self: &Arc<Self>, action: &Value) {
        let id = action["id"].as_str().unwrap().to_owned();
        let tab = action["tab"].as_u64().unwrap().to_string();
        let kind = action["kind"].as_str().unwrap().to_owned();
        let spawn = action.get("spawn").cloned();
        self.progress.lock().unwrap().operations.push(json!({
            "id":id,"tab":action["tab"],"started":false,"status":"pending"
        }));
        let (release, receive) = mpsc::channel();
        self.releases.lock().unwrap().insert(id.clone(), release);
        let harness = self.clone();
        let worker = thread::spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let operation = harness.state.acquire(&tab);
                let operation = if kind == "internal" {
                    operation.begin_internal()
                } else {
                    Some(operation)
                };
                let Some(operation) = operation else {
                    return Ok(Value::Null);
                };
                {
                    let mut progress = harness.progress.lock().unwrap();
                    progress.started.push(id.clone());
                    progress
                        .operations
                        .iter_mut()
                        .find(|row| row["id"] == id)
                        .unwrap()["started"] = json!(true);
                }
                if let Some(spawn) = &spawn {
                    harness.enqueue(spawn);
                }
                if kind == "throw" {
                    panic!("fixture refusal");
                }
                if receive
                    .recv_timeout(Duration::from_secs(5))
                    .expect("missing callback release")
                {
                    return Err(());
                }
                match kind.as_str() {
                    "raw_start" => operation.raw_succeeded("Page.startScreencast"),
                    "raw_stop" => operation.raw_succeeded("Page.stopScreencast"),
                    _ => {}
                }
                Ok(json!(id))
            }));
            let (status, value) = match outcome {
                Ok(Ok(value)) => ("fulfilled", value),
                Ok(Err(())) => ("rejected", json!("fixture refusal")),
                Err(payload) => {
                    let message = payload
                        .downcast_ref::<&str>()
                        .copied()
                        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                        .unwrap_or("unexpected native panic");
                    ("rejected", json!(message))
                }
            };
            let mut progress = harness.progress.lock().unwrap();
            let row = progress
                .operations
                .iter_mut()
                .find(|row| row["id"] == id)
                .unwrap();
            row["status"] = json!(status);
            row["value"] = value;
        });
        self.threads.lock().unwrap().push(worker);
    }
    fn snapshot(&self) -> (Value, BTreeMap<String, usize>) {
        let tabs = self.state.tabs();
        let progress = self.progress.lock().unwrap();
        let selected = |predicate: fn(&Tab) -> bool| {
            tabs.iter()
                .filter(|(_, tab)| predicate(tab))
                .map(|(tab, _)| tab.parse::<u64>().unwrap())
                .collect::<Vec<_>>()
        };
        let counts = tabs
            .iter()
            .filter(|(_, tab)| !tab.queue.is_empty())
            .map(|(tab, state)| (tab.clone(), state.queue.len()))
            .collect();
        (
            json!({"started":progress.started,"operations":progress.operations,
            "queuedTabs":selected(|tab| !tab.queue.is_empty()),
            "rawTabs":selected(|tab| tab.raw),
            "activeTabs":selected(|tab| tab.active.is_some())}),
            counts,
        )
    }
    fn stop_and_join(&self) {
        for release in self.releases.lock().unwrap().values() {
            let _ = release.send(false);
        }
        loop {
            let threads: Vec<_> = self.threads.lock().unwrap().drain(..).collect();
            if threads.is_empty() {
                break;
            }
            for worker in threads {
                worker.join().unwrap();
            }
        }
    }
}

#[test]
fn original_screencast_fifo_callback_schedules_match() {
    let oracle: Value = serde_json::from_str(include_str!(
        "../tests/oracles/browser_screencast_queue.json"
    ))
    .unwrap();
    for case in oracle["cases"].as_array().unwrap() {
        let harness = Arc::new(Harness::default());
        for (action, expected) in case["actions"]
            .as_array()
            .unwrap()
            .iter()
            .zip(case["rows"].as_array().unwrap())
        {
            match action["op"].as_str().unwrap() {
                "enqueue" => harness.enqueue(action),
                "finish" => harness.releases.lock().unwrap()[action["id"].as_str().unwrap()]
                    .send(action["fail"] == true)
                    .unwrap(),
                "forget" => harness
                    .state
                    .forget(&action["tab"].as_u64().unwrap().to_string()),
                op => panic!("unknown queue action {op}"),
            }
            // Count every unsettled original task, not just its tab-map key.
            // This ensures a Rust waiter has actually registered before the
            // harness submits the next operation for that same tab.
            let mut pending = BTreeMap::new();
            for row in expected["operations"].as_array().unwrap() {
                if row["status"] == "pending" {
                    *pending
                        .entry(row["tab"].as_u64().unwrap().to_string())
                        .or_insert(0usize) += 1;
                }
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let (observed, counts) = harness.snapshot();
                if observed == *expected && counts == pending {
                    break;
                }
                if Instant::now() >= deadline {
                    harness.stop_and_join();
                    panic!(
                        "{}: {action}\nobserved={observed}\nexpected={expected}\ncounts={counts:?}, pending={pending:?}",
                        case["name"]
                    );
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
        harness.stop_and_join();
        assert!(
            harness
                .state
                .tabs()
                .values()
                .all(|tab| tab.queue.is_empty())
        );
    }
}
