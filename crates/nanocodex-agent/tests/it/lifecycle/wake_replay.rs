use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use nanocodex_agent::{
    execution::{
        ExecutionAdmission, ExecutionContinuation, ExecutionFuture, ExecutionOutput,
        ExecutionPolicy, ExecutionStepAdmission,
    },
    session::SessionSnapshot,
};
use nanocodex_oai_api::responses::FunctionOutputBody;

use super::*;

#[derive(Default)]
struct WakeJournal {
    pending_snapshot: Mutex<Option<SessionSnapshot>>,
    wake_input: Mutex<Option<serde_json::Value>>,
    completed: Mutex<Option<(SessionSnapshot, ExecutionOutput)>>,
    lose_completion_ack: AtomicBool,
    recovered_failures: AtomicUsize,
    retriable_wake_attempts: AtomicUsize,
    cancelled_wake: AtomicBool,
}

impl ExecutionPolicy for WakeJournal {
    fn recover_failure<'a>(
        &'a self,
        operation_id: String,
        error: nanocodex_agent::NanocodexError,
    ) -> ExecutionFuture<'a, nanocodex_agent::NanocodexError> {
        Box::pin(async move {
            if operation_id.starts_with("late-continuation:") {
                self.recovered_failures.fetch_add(1, Ordering::SeqCst);
            }
            error
        })
    }

    fn admit<'a>(
        &'a self,
        operation_id: String,
        input: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<ExecutionAdmission>> {
        Box::pin(async move {
            if operation_id.starts_with("late-continuation:") {
                *self.wake_input.lock().unwrap() = Some(serde_json::from_str(&input).unwrap());
                if self.cancelled_wake.load(Ordering::SeqCst) {
                    return Ok(ExecutionAdmission::Cancelled);
                }
            }
            if operation_id.starts_with("late-continuation:")
                && let Some((snapshot, output)) = self.completed.lock().unwrap().clone()
            {
                return Ok(ExecutionAdmission::Completed { snapshot, output });
            }
            Ok(ExecutionAdmission::Execute)
        })
    }

    fn admit_automatic<'a>(
        &'a self,
        operation_id: String,
        input: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<(String, ExecutionAdmission)>> {
        Box::pin(async move { Ok((operation_id.clone(), self.admit(operation_id, input).await?)) })
    }

    fn release<'a>(&'a self, _operation_id: String) -> ExecutionFuture<'a, ()> {
        Box::pin(async {})
    }

    fn begin_attempt<'a>(
        &'a self,
        _operation_id: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn continuation<'a>(
        &'a self,
        _operation_id: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<Option<ExecutionContinuation>>> {
        Box::pin(async { Ok(None) })
    }

    fn advance<'a>(
        &'a self,
        _operation_id: String,
        _state: ExecutionContinuation,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn begin_step<'a>(
        &'a self,
        _operation_id: String,
        _step_id: String,
        _kind: String,
        _input: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<ExecutionStepAdmission>> {
        Box::pin(async { Ok(ExecutionStepAdmission::Execute) })
    }

    fn complete_step<'a>(
        &'a self,
        _operation_id: String,
        _step_id: String,
        _output: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn commit_checkpoint<'a>(
        &'a self,
        snapshot: SessionSnapshot,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        Box::pin(async move {
            *self.pending_snapshot.lock().unwrap() = Some(snapshot);
            Ok(())
        })
    }

    fn complete<'a>(
        &'a self,
        operation_id: String,
        snapshot: SessionSnapshot,
        output: ExecutionOutput,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        Box::pin(async move {
            if operation_id.starts_with("late-output:") {
                *self.pending_snapshot.lock().unwrap() = Some(snapshot);
            } else if operation_id.starts_with("late-continuation:") {
                *self.completed.lock().unwrap() = Some((snapshot, output));
                if self.lose_completion_ack.swap(false, Ordering::SeqCst) {
                    return Err(nanocodex_agent::NanocodexError::InvalidExecutionPolicy(
                        "simulated crash after journal commit, before driver publication".into(),
                    ));
                }
            }
            Ok(())
        })
    }

    fn cancel<'a>(
        &'a self,
        operation_id: String,
        _snapshot: Option<SessionSnapshot>,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        Box::pin(async move {
            if operation_id.starts_with("late-continuation:") {
                self.cancelled_wake.store(true, Ordering::SeqCst);
            }
            Ok(())
        })
    }

    fn fail_attempt<'a>(
        &'a self,
        operation_id: String,
        _error: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        Box::pin(async move {
            if operation_id.starts_with("late-continuation:") {
                self.retriable_wake_attempts.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        })
    }

    fn fail<'a>(
        &'a self,
        _operation_id: String,
        _snapshot: SessionSnapshot,
        _error: String,
    ) -> ExecutionFuture<'a, nanocodex_agent::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn shutdown_before_wake_response_preserves_retriable_same_id_delivery() {
    let (seed_attempts, _seed_rx) = mpsc::unbounded_channel();
    let openai = OpenAi::builder("test")
        .service(move || RetainingCompletedService {
            retained: seed_attempts.clone(),
        })
        .build()
        .unwrap();
    let (seed, seed_events) = Nanocodex::builder(openai)
        .tools(Tools::builder().without_defaults().build().unwrap())
        .build()
        .unwrap();
    seed.prompt("seed").await.unwrap().result().await.unwrap();
    let mut snapshot = serde_json::to_value(seed.snapshot().await.unwrap()).unwrap();
    seed.shutdown().await.unwrap();
    drop(seed_events);
    snapshot["unreal_function_outputs"] = serde_json::json!(true);
    snapshot["history"].as_array_mut().unwrap().extend([
        serde_json::json!({
            "type":"function_call", "call_id":"job-shutdown", "name":"job", "arguments":"{}"
        }),
        serde_json::json!({
            "type":"function_call_output", "call_id":"job-shutdown",
            "output":"Tool call is still running. Its result arrives in a later turn: continue with independent work, or end your turn to wait for it."
        }),
    ]);
    let journal = Arc::new(WakeJournal::default());
    let started = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let pending_openai = OpenAi::builder("test")
        .service({
            let started = Arc::clone(&started);
            let dropped = Arc::clone(&dropped);
            move || DropPendingService {
                started: Arc::clone(&started),
                dropped: Arc::clone(&dropped),
            }
        })
        .build()
        .unwrap();
    let tools = Tools::builder().without_defaults().build().unwrap();
    let (first, first_events) = Nanocodex::builder(pending_openai)
        .resume(serde_json::from_value(snapshot).unwrap())
        .execution_policy(journal.clone())
        .tools(tools.clone())
        .build()
        .unwrap();
    first
        .submit_late_function_output(
            "job-shutdown",
            FunctionOutputBody::Text("terminal after crash".into()),
            "operation-shutdown",
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !started.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("wake request should begin before shutdown");
    let staged = journal.pending_snapshot.lock().unwrap().clone().unwrap();
    let wake_id = serde_json::to_value(&staged).unwrap()["pending_late_wake"]
        .as_str()
        .unwrap()
        .to_owned();
    tokio::time::timeout(Duration::from_secs(5), first.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(dropped.load(Ordering::Acquire));
    assert_eq!(journal.retriable_wake_attempts.load(Ordering::SeqCst), 1);
    assert!(!journal.cancelled_wake.load(Ordering::SeqCst));
    assert!(journal.completed.lock().unwrap().is_none());
    drop(first_events);

    let (attempts, mut observed) = mpsc::unbounded_channel();
    let openai = OpenAi::builder("test")
        .service(move || RetainingCompletedService {
            retained: attempts.clone(),
        })
        .build()
        .unwrap();
    let (restarted, events) = Nanocodex::builder(openai)
        .resume(staged)
        .execution_policy(journal.clone())
        .tools(tools)
        .build()
        .unwrap();
    let request = tokio::time::timeout(Duration::from_secs(5), observed.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(request.input_items().any(|item| {
        serde_json::to_string(item)
            .unwrap()
            .contains("terminal after crash")
    }));
    let restored = restarted.snapshot().await.unwrap();
    let restored = serde_json::to_value(restored).unwrap();
    assert!(restored["pending_late_wake"].is_null());
    assert_eq!(
        journal.wake_input.lock().unwrap().as_ref().unwrap()["wake_id"],
        wake_id
    );
    restarted.shutdown().await.unwrap();
    drop(events);
}

#[tokio::test]
async fn completed_wake_admission_restores_authoritative_snapshot_and_live_model() {
    // Seed an opted-in staged function output without depending on a real tool.
    let (seed_attempts, _seed_rx) = mpsc::unbounded_channel();
    let openai = OpenAi::builder("test")
        .service(move || RetainingCompletedService {
            retained: seed_attempts.clone(),
        })
        .build()
        .unwrap();
    let (seed, events) = Nanocodex::builder(openai)
        .tools(Tools::builder().without_defaults().build().unwrap())
        .build()
        .unwrap();
    seed.prompt("seed").await.unwrap().result().await.unwrap();
    let mut seed_snapshot = serde_json::to_value(seed.snapshot().await.unwrap()).unwrap();
    seed.shutdown().await.unwrap();
    drop(events);
    seed_snapshot["unreal_function_outputs"] = serde_json::json!(true);
    let history = seed_snapshot["history"].as_array_mut().unwrap();
    history.push(serde_json::json!({
        "type":"function_call", "call_id":"job-replay", "name":"job", "arguments":"{}"
    }));
    history.push(serde_json::json!({
        "type":"function_call_output", "call_id":"job-replay",
        "output":"Tool call is still running. Its result arrives in a later turn: continue with independent work, or end your turn to wait for it."
    }));
    let seed_snapshot = serde_json::from_value(seed_snapshot).unwrap();
    let journal = Arc::new(WakeJournal::default());
    journal.lose_completion_ack.store(true, Ordering::SeqCst);
    let (attempts, mut observed) = mpsc::unbounded_channel();
    let openai = OpenAi::builder("test")
        .service(move || RetainingCompletedService {
            retained: attempts.clone(),
        })
        .build()
        .unwrap();
    let tools = Tools::builder().without_defaults().build().unwrap();
    let (first, events) = Nanocodex::builder(openai.clone())
        .resume(seed_snapshot)
        .execution_policy(journal.clone())
        .tools(tools.clone())
        .build()
        .unwrap();
    first
        .submit_late_function_output(
            "job-replay",
            FunctionOutputBody::Text("terminal result".into()),
            "replay-operation",
        )
        .await
        .unwrap();
    let pending = journal.pending_snapshot.lock().unwrap().clone().unwrap();
    assert_eq!(
        serde_json::to_value(&pending).unwrap()["pending_late_jobs"],
        serde_json::json!([{"job_id":"replay-operation", "call_id":"job-replay"}])
    );
    let first_wake = tokio::time::timeout(Duration::from_secs(5), observed.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        journal.wake_input.lock().unwrap().as_ref().unwrap()["jobs"],
        serde_json::json!([{"job_id":"replay-operation", "call_id":"job-replay"}])
    );
    assert!(first_wake.input_items().any(|item| {
        serde_json::to_string(item)
            .unwrap()
            .contains("terminal result")
    }));
    // The journal commits the wake but loses its acknowledgment. The driver
    // cannot publish the checkpoint, modeling a crash at exactly that gap.
    let authoritative = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some((snapshot, _)) = journal.completed.lock().unwrap().clone() {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        first.shutdown().await.is_err(),
        "the first driver lost its acknowledgment"
    );
    assert_eq!(
        journal.recovered_failures.load(Ordering::SeqCst),
        1,
        "the policy must reconcile a failure after its commit succeeded"
    );
    drop(events);
    let (restarted, events) = Nanocodex::builder(openai)
        .resume(pending)
        .execution_policy(journal)
        .tools(tools)
        .build()
        .unwrap();
    let restored = tokio::time::timeout(Duration::from_secs(5), restarted.snapshot())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(restored).unwrap(),
        serde_json::to_value(&authoritative).unwrap(),
        "the journal snapshot, not the stale live checkpoint, must win"
    );
    assert!(
        observed.try_recv().is_err(),
        "replay must not call the provider"
    );
    restarted
        .prompt("after recovered wake")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let next = observed
        .try_recv()
        .expect("subsequent prompt calls provider");
    let replayed_history = next
        .input_items()
        .map(|item| serde_json::to_string(item).unwrap())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(replayed_history.contains("terminal result"));
    assert!(replayed_history.contains("after recovered wake"));
    restarted.shutdown().await.unwrap();
    drop(events);
}

#[tokio::test]
async fn completed_wake_from_another_lineage_fails_closed_without_provider_call() {
    let (seed_attempts, _seed_rx) = mpsc::unbounded_channel();
    let openai = OpenAi::builder("test")
        .service(move || RetainingCompletedService {
            retained: seed_attempts.clone(),
        })
        .build()
        .unwrap();
    let (seed, events) = Nanocodex::builder(openai)
        .tools(Tools::builder().without_defaults().build().unwrap())
        .build()
        .unwrap();
    seed.prompt("seed").await.unwrap().result().await.unwrap();
    let mut stale = serde_json::to_value(seed.snapshot().await.unwrap()).unwrap();
    seed.shutdown().await.unwrap();
    drop(events);
    stale["pending_late_wake"] = serde_json::json!("pending-replay");
    let mut alien = stale.clone();
    alien["lineage_id"] = serde_json::json!("another-conversation");
    alien["pending_late_wake"] = serde_json::Value::Null;
    let journal = Arc::new(WakeJournal::default());
    *journal.completed.lock().unwrap() = Some((
        serde_json::from_value(alien).unwrap(),
        ExecutionOutput {
            final_message: "not ours".into(),
            usage: Default::default(),
        },
    ));
    let openai = OpenAi::builder("test")
        .service(|| NeverCalled)
        .build()
        .unwrap();
    let (restarted, events) = Nanocodex::builder(openai)
        .resume(serde_json::from_value(stale).unwrap())
        .execution_policy(journal)
        .tools(Tools::builder().without_defaults().build().unwrap())
        .build()
        .unwrap();
    assert!(
        restarted.snapshot().await.is_err(),
        "incompatible admission must stop driver"
    );
    let failure = restarted.shutdown().await.unwrap_err().to_string();
    assert!(failure.contains("another lineage or model"), "{failure}");
    drop(events);
}
