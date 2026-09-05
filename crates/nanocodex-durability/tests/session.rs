use nanocodex_durability::{
    Admission, BeginStep, DurableSession, Error, MemoryStore, OwnedState, OwnerId, OwnerToken,
    StateStore, StepStatus, StoreError, StoreFuture, StoredState,
};
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Deserialize, Serialize)]
struct PromptInput {
    prompt: String,
}

#[derive(Deserialize, Serialize)]
struct ModelInput {
    history: u32,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct ModelOutput {
    answer: u32,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct Checkpoint {
    version: u32,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct TurnOutput {
    message: String,
}

struct CommitThenFailStore {
    inner: MemoryStore,
    fail_after_revision: u64,
}

struct NotCommittedOnceStore {
    inner: MemoryStore,
    fail_at_revision: u64,
    failed: Arc<AtomicBool>,
}

struct SeededStore {
    state: StoredState,
}

#[derive(Clone, Copy)]
enum StepGateMoment {
    BeforeStartCommit,
    AfterCompletionCommit,
}

struct StepGateStore {
    inner: MemoryStore,
    moment: StepGateMoment,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    used: Arc<AtomicBool>,
}

impl StateStore for StepGateStore {
    fn acquire<'a>(
        &'a mut self,
        state_id: &'a str,
        owner_id: OwnerId,
    ) -> StoreFuture<'a, Result<OwnedState, StoreError>> {
        self.inner.acquire(state_id, owner_id)
    }

    fn replace<'a>(
        &'a mut self,
        state_id: &'a str,
        owner: &'a OwnerToken,
        expected_revision: u64,
        payload: &'a str,
    ) -> StoreFuture<'a, Result<u64, StoreError>> {
        let matches = match self.moment {
            StepGateMoment::BeforeStartCommit => payload.contains("\"status\":\"effect_pending\""),
            StepGateMoment::AfterCompletionCommit => payload.contains("\"status\":{\"completed\":"),
        };
        if matches && !self.used.swap(true, Ordering::SeqCst) {
            let entered = Arc::clone(&self.entered);
            let release = Arc::clone(&self.release);
            return match self.moment {
                StepGateMoment::BeforeStartCommit => Box::pin(async move {
                    entered.notify_one();
                    release.notified().await;
                    self.inner
                        .replace(state_id, owner, expected_revision, payload)
                        .await
                }),
                StepGateMoment::AfterCompletionCommit => Box::pin(async move {
                    let revision = self
                        .inner
                        .replace(state_id, owner, expected_revision, payload)
                        .await?;
                    entered.notify_one();
                    release.notified().await;
                    Ok(revision)
                }),
            };
        }
        self.inner
            .replace(state_id, owner, expected_revision, payload)
    }
}

impl StateStore for SeededStore {
    fn acquire<'a>(
        &'a mut self,
        _state_id: &'a str,
        owner_id: OwnerId,
    ) -> StoreFuture<'a, Result<OwnedState, StoreError>> {
        let state = self.state.clone();
        Box::pin(async move {
            Ok(OwnedState {
                owner: OwnerToken::new(owner_id, 1),
                state,
            })
        })
    }

    fn replace<'a>(
        &'a mut self,
        _state_id: &'a str,
        _owner: &'a OwnerToken,
        _expected_revision: u64,
        _payload: &'a str,
    ) -> StoreFuture<'a, Result<u64, StoreError>> {
        Box::pin(async {
            Err(StoreError::Backend(
                "the seeded store is read-only".to_owned(),
            ))
        })
    }
}

impl StateStore for NotCommittedOnceStore {
    fn acquire<'a>(
        &'a mut self,
        state_id: &'a str,
        owner_id: OwnerId,
    ) -> StoreFuture<'a, Result<OwnedState, StoreError>> {
        self.inner.acquire(state_id, owner_id)
    }

    fn replace<'a>(
        &'a mut self,
        state_id: &'a str,
        owner: &'a OwnerToken,
        expected_revision: u64,
        payload: &'a str,
    ) -> StoreFuture<'a, Result<u64, StoreError>> {
        if expected_revision == self.fail_at_revision && !self.failed.swap(true, Ordering::SeqCst) {
            return Box::pin(async {
                Err(StoreError::NotCommitted(
                    "injected retryable replacement failure".to_owned(),
                ))
            });
        }
        self.inner
            .replace(state_id, owner, expected_revision, payload)
    }
}

impl StateStore for CommitThenFailStore {
    fn acquire<'a>(
        &'a mut self,
        state_id: &'a str,
        owner_id: OwnerId,
    ) -> StoreFuture<'a, Result<OwnedState, StoreError>> {
        self.inner.acquire(state_id, owner_id)
    }

    fn replace<'a>(
        &'a mut self,
        state_id: &'a str,
        owner: &'a OwnerToken,
        expected_revision: u64,
        payload: &'a str,
    ) -> StoreFuture<'a, Result<u64, StoreError>> {
        Box::pin(async move {
            let revision = self
                .inner
                .replace(state_id, owner, expected_revision, payload)
                .await?;
            if expected_revision == self.fail_after_revision {
                return Err(StoreError::Backend(
                    "replacement response was lost after commit".to_owned(),
                ));
            }
            Ok(revision)
        })
    }
}

#[test]
fn memory_store_requires_an_owner_runtime() {
    assert!(matches!(MemoryStore::new(), Err(Error::RuntimeUnavailable)));
}

#[tokio::test]
async fn replays_completed_operations_and_steps_after_reopen() {
    let store = MemoryStore::new().unwrap();
    let session = DurableSession::open(store.clone(), "session")
        .await
        .unwrap();
    assert!(matches!(
        session
            .admit_typed::<_, Checkpoint, TurnOutput>(
                "turn-1",
                &PromptInput {
                    prompt: "hi".to_owned(),
                },
            )
            .await,
        Ok(Admission::Accepted)
    ));
    session.begin_attempt("turn-1").await.unwrap();
    assert!(matches!(
        session
            .begin_step_typed::<_, ModelOutput>(
                "turn-1",
                "model-1",
                "model",
                &ModelInput { history: 0 },
            )
            .await,
        Ok(BeginStep::Execute)
    ));
    session
        .complete_step("turn-1", "model-1", &ModelOutput { answer: 42 })
        .await
        .unwrap();
    session
        .complete(
            "turn-1",
            &Checkpoint { version: 1 },
            &TurnOutput {
                message: "done".to_owned(),
            },
        )
        .await
        .unwrap();

    let reopened = DurableSession::open(store, "session").await.unwrap();
    let admission = reopened
        .admit_typed::<_, Checkpoint, TurnOutput>(
            "turn-1",
            &PromptInput {
                prompt: "hi".to_owned(),
            },
        )
        .await
        .unwrap();
    let Admission::Completed { checkpoint, output } = admission else {
        panic!("completed operation must replay typed terminal values");
    };
    assert_eq!(checkpoint, Checkpoint { version: 1 });
    assert_eq!(output.message, "done");
    assert_eq!(reopened.state().await.unwrap().revision(), 4);
}

#[tokio::test]
async fn failed_operations_replay_their_error_and_do_not_block_follow_on_work() {
    let store = MemoryStore::new().unwrap();
    let session = DurableSession::open(store.clone(), "failed-session")
        .await
        .unwrap();
    let input = PromptInput {
        prompt: "bad image".to_owned(),
    };
    session.admit("turn-1", &input).await.unwrap();
    session.begin_attempt("turn-1").await.unwrap();
    session
        .fail("turn-1", &Checkpoint { version: 2 }, "invalid image")
        .await
        .unwrap();

    let replay = session
        .admit_typed::<_, Checkpoint, TurnOutput>("turn-1", &input)
        .await
        .unwrap();
    let Admission::Failed { checkpoint, error } = replay else {
        panic!("failed operation must replay its terminal checkpoint and error");
    };
    assert_eq!(checkpoint, Checkpoint { version: 2 });
    assert_eq!(error, "invalid image");

    session.admit("turn-2", &"continue").await.unwrap();
    session.cancel("turn-2").await.unwrap();

    let reopened = DurableSession::open(store, "failed-session").await.unwrap();
    let checkpoint = reopened
        .latest_checkpoint()
        .await
        .unwrap()
        .expect("failed operation checkpoint");
    assert_eq!(
        checkpoint.decode::<Checkpoint>().unwrap(),
        Checkpoint { version: 2 }
    );
}

#[tokio::test]
async fn retries_an_unfinished_step_after_reopen() {
    let store = MemoryStore::new().unwrap();
    let session = DurableSession::open(store.clone(), "session")
        .await
        .unwrap();
    session.admit("turn-1", &"hi").await.unwrap();
    session.begin_attempt("turn-1").await.unwrap();
    session
        .begin_step("turn-1", "tool-1", "tool", &"charge")
        .await
        .unwrap();

    let reopened = DurableSession::open(store, "session").await.unwrap();
    assert!(matches!(
        reopened.admit("turn-1", &"hi").await,
        Ok(Admission::Pending)
    ));
    reopened.begin_attempt("turn-1").await.unwrap();
    assert!(matches!(
        reopened
            .begin_step("turn-1", "tool-1", "tool", &"charge")
            .await,
        Ok(BeginStep::Execute)
    ));
    assert_eq!(
        reopened
            .state()
            .await
            .unwrap()
            .operation("turn-1")
            .unwrap()
            .steps["tool-1"]
            .attempts,
        2
    );
}

#[tokio::test]
async fn aborted_begin_caller_does_not_cancel_the_owned_store_commit() {
    let inner = MemoryStore::new().unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let session = Arc::new(
        DurableSession::open(
            StepGateStore {
                inner,
                moment: StepGateMoment::BeforeStartCommit,
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
                used: Arc::new(AtomicBool::new(false)),
            },
            "cancel-before-begin-observed",
        )
        .await
        .unwrap(),
    );
    session.admit("turn-1", &"run tool").await.unwrap();
    session.begin_attempt("turn-1").await.unwrap();
    let beginning = tokio::spawn({
        let session = Arc::clone(&session);
        async move {
            session
                .begin_step("turn-1", "tool-1", "tool_call", &"effect")
                .await
        }
    });
    entered.notified().await;
    beginning.abort();
    assert!(beginning.await.unwrap_err().is_cancelled());
    release.notify_one();

    let state = session.state().await.unwrap();
    let step = &state.operation("turn-1").unwrap().steps["tool-1"];
    assert!(matches!(step.status, StepStatus::EffectPending));
    assert_eq!(step.attempts, 1);
}

#[tokio::test]
async fn an_unfinished_step_can_execute_again_in_the_same_attempt() {
    let session = DurableSession::open(
        MemoryStore::new().unwrap(),
        "cancel-authorized-pending-step",
    )
    .await
    .unwrap();
    session.admit("turn-1", &"run tool").await.unwrap();
    session.begin_attempt("turn-1").await.unwrap();
    assert!(matches!(
        session
            .begin_step("turn-1", "tool-1", "tool_call", &"effect",)
            .await
            .unwrap(),
        BeginStep::Execute
    ));

    assert!(matches!(
        session
            .begin_step("turn-1", "tool-1", "tool_call", &"effect")
            .await,
        Ok(BeginStep::Execute)
    ));
    let state = session.state().await.unwrap();
    assert_eq!(
        state.operation("turn-1").unwrap().steps["tool-1"].attempts,
        2
    );
    assert!(matches!(
        state.operation("turn-1").unwrap().steps["tool-1"].status,
        StepStatus::EffectPending
    ));
}

#[tokio::test]
async fn begin_waits_for_its_store_commit() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let session = Arc::new(
        DurableSession::open(
            StepGateStore {
                inner: MemoryStore::new().unwrap(),
                moment: StepGateMoment::BeforeStartCommit,
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
                used: Arc::new(AtomicBool::new(false)),
            },
            "cancel-live-begin-handoff",
        )
        .await
        .unwrap(),
    );
    session.admit("turn-1", &"run tool").await.unwrap();
    session.begin_attempt("turn-1").await.unwrap();
    let beginning = tokio::spawn({
        let session = Arc::clone(&session);
        async move {
            session
                .begin_step("turn-1", "tool-1", "tool_call", &"effect")
                .await
        }
    });
    entered.notified().await;
    release.notify_one();
    assert!(matches!(
        beginning.await.unwrap().unwrap(),
        BeginStep::Execute
    ));
}

#[tokio::test]
async fn completion_committed_before_its_reply_is_observed_replays_after_reopen() {
    let inner = MemoryStore::new().unwrap();
    let store = inner.clone();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let session = Arc::new(
        DurableSession::open(
            StepGateStore {
                inner,
                moment: StepGateMoment::AfterCompletionCommit,
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
                used: Arc::new(AtomicBool::new(false)),
            },
            "cancel-after-completion-commit",
        )
        .await
        .unwrap(),
    );
    session.admit("turn-1", &"run tool").await.unwrap();
    session.begin_attempt("turn-1").await.unwrap();
    session
        .begin_step("turn-1", "tool-1", "tool_call", &"effect")
        .await
        .unwrap();

    let completing = tokio::spawn({
        let session = Arc::clone(&session);
        async move {
            session
                .complete_step("turn-1", "tool-1", &"real output")
                .await
        }
    });
    entered.notified().await;
    completing.abort();
    assert!(completing.await.unwrap_err().is_cancelled());
    release.notify_one();
    drop(session);
    let reopened = DurableSession::open(store, "cancel-after-completion-commit")
        .await
        .unwrap();
    assert!(matches!(
        reopened.admit("turn-1", &"run tool").await,
        Ok(Admission::Pending)
    ));
    reopened.begin_attempt("turn-1").await.unwrap();
    let BeginStep::Replay(output) = reopened
        .begin_step("turn-1", "tool-1", "tool_call", &"effect")
        .await
        .unwrap()
    else {
        panic!("the committed real output must replay")
    };
    assert_eq!(output.decode::<String>().unwrap(), "real output");
}

#[tokio::test]
async fn queues_admission_but_serializes_attempts() {
    let store = MemoryStore::new().unwrap();
    let session = DurableSession::open(store, "session").await.unwrap();
    session.admit("turn-1", &"one").await.unwrap();
    session.admit("turn-2", &"two").await.unwrap();
    assert!(matches!(
        session.begin_attempt("turn-2").await,
        Err(Error::OperationBlocked { .. })
    ));
    session.begin_attempt("turn-1").await.unwrap();
    session.complete("turn-1", &1, &"one").await.unwrap();
    session.begin_attempt("turn-2").await.unwrap();
}

fn seeded_store(payloads: &[&str]) -> SeededStore {
    assert_eq!(payloads.len(), 1, "durable state has no replay history");
    SeededStore {
        state: StoredState {
            revision: 1,
            payload: Some(payloads[0].to_owned()),
        },
    }
}

#[tokio::test]
async fn rejects_every_inconsistent_store_state_shape() {
    for (name, state) in [
        (
            "payload-at-zero",
            StoredState {
                revision: 0,
                payload: Some("{}".to_owned()),
            },
        ),
        (
            "missing-at-nonzero",
            StoredState {
                revision: 1,
                payload: None,
            },
        ),
    ] {
        let error = match DurableSession::open(SeededStore { state }, name).await {
            Ok(_) => panic!("inconsistent retained state was accepted"),
            Err(error) => error,
        };
        assert!(matches!(error, Error::InvalidState(_)));
    }
}

#[tokio::test]
async fn rejects_unknown_outer_state_fields() {
    let error = match DurableSession::open(
        seeded_store(&[r#"{"nanocodex_durable_state":{"format":2,"operations":{},"latest_checkpoint":null},"unknown":true}"#]),
        "unknown-outer-field",
    )
    .await
    {
        Ok(_) => panic!("unknown retained state field was accepted"),
        Err(error) => error,
    };
    assert!(matches!(error, Error::Decode { revision: 1, .. }));
}

#[tokio::test]
async fn rejects_noncanonical_checkpoint_fields() {
    let payload = r#"{"nanocodex_durable_state":{"format":2,"operations":{},"latest_checkpoint":null,"generation":1}}"#;
    let error = match DurableSession::open(
        SeededStore {
            state: StoredState {
                revision: 1,
                payload: Some(payload.to_owned()),
            },
        },
        "noncanonical-checkpoint",
    )
    .await
    {
        Ok(_) => panic!("checkpoint fields must match the current protocol exactly"),
        Err(error) => error,
    };

    assert!(matches!(error, Error::Decode { revision: 1, .. }));
}

#[tokio::test]
async fn rejects_retry_attempt_counter_overflow_without_advancing_state() {
    let revision = u64::from(u32::MAX);
    let payload = format!(
        r#"{{"nanocodex_durable_state":{{"format":2,"operations":{{"turn":{{"input":"\"prompt\"","status":"pending","steps":{{"model":{{"kind":"model","input":"\"retry\"","status":"effect_pending","attempts":{}}}}},"accepted_order":1}}}},"latest_checkpoint":null}}}}"#,
        u32::MAX,
    );
    let session = DurableSession::open(
        SeededStore {
            state: StoredState {
                revision,
                payload: Some(payload),
            },
        },
        "attempt-overflow",
    )
    .await
    .unwrap();
    assert!(matches!(
        session.admit("turn", &"prompt").await,
        Ok(Admission::Pending)
    ));
    session.begin_attempt("turn").await.unwrap();

    let error = session
        .begin_step("turn", "model", "model", &"retry")
        .await
        .expect_err("attempt overflow must not silently saturate");
    assert!(matches!(error, Error::InvalidState(_)));
    assert_eq!(session.state().await.unwrap().revision(), revision);
}

#[tokio::test]
async fn rejects_the_deleted_journal_state_envelope() {
    let payload = r#"{"nanocodex_journal_state":{"format":1,"operations":{},"latest_checkpoint":null,"checkpoint_effect_pending":false}}"#;
    let error = match DurableSession::open(seeded_store(&[payload]), "deleted-state-envelope").await
    {
        Ok(_) => panic!("the current-state protocol must not adopt old state state"),
        Err(error) => error,
    };

    assert!(matches!(error, Error::Decode { revision: 1, .. }));
}

#[tokio::test]
async fn rejects_the_previous_state_format() {
    let payload =
        r#"{"nanocodex_durable_state":{"format":1,"operations":{},"latest_checkpoint":null}}"#;
    let error = match DurableSession::open(seeded_store(&[payload]), "previous-state-format").await
    {
        Ok(_) => panic!("the retry-only protocol must not adopt the previous state format"),
        Err(error) => error,
    };

    assert!(matches!(error, Error::InvalidState(_)));
}

#[tokio::test]
async fn rejects_noncanonical_transition_shape() {
    let error = match DurableSession::open(
        seeded_store(&[r#"{"operation_accepted":{"operation_id":"turn","input":"prompt"}}"#]),
        "noncanonical-entry",
    )
    .await
    {
        Ok(_) => panic!("state entries must use the current tagged shape"),
        Err(error) => error,
    };

    assert!(matches!(error, Error::Decode { revision: 1, .. }));
}

#[tokio::test]
async fn rejects_a_checkpoint_terminal_that_crosses_pending_work() {
    let payload = r#"{"nanocodex_durable_state":{"format":2,"operations":{"turn-1":{"input":"\"first\"","status":"pending","steps":{},"accepted_order":1},"turn-2":{"input":"\"second\"","status":{"completed":{"checkpoint":"\"crossed\"","output":"\"done\""}},"steps":{},"accepted_order":2}},"latest_checkpoint":"\"crossed\""}}"#;
    let error = match DurableSession::open(
        SeededStore {
            state: StoredState {
                revision: 2,
                payload: Some(payload.to_owned()),
            },
        },
        "crossed-checkpoint-terminal",
    )
    .await
    {
        Ok(_) => panic!("checkpoint-bearing terminals must preserve operation order"),
        Err(error) => error,
    };

    assert!(matches!(error, Error::InvalidState(_)));
}

#[tokio::test]
async fn rejects_the_deleted_checkpoint_effect_field() {
    let payload = r#"{"nanocodex_durable_state":{"format":2,"operations":{"turn":{"input":"\"prompt\"","status":"pending","steps":{},"accepted_order":1}},"latest_checkpoint":null,"checkpoint_effect_pending":true}}"#;
    let error =
        match DurableSession::open(seeded_store(&[payload]), "crossed-checkpoint-effect").await {
            Ok(_) => panic!("standalone checkpoint effects must not cross pending operations"),
            Err(error) => error,
        };

    assert!(matches!(error, Error::Decode { revision: 1, .. }));
}

#[tokio::test]
async fn reopens_a_restarted_step() {
    let payload = r#"{"nanocodex_durable_state":{"format":2,"operations":{"turn":{"input":"\"prompt\"","status":"pending","steps":{"tool-1":{"kind":"tool","input":"\"charge\"","status":"effect_pending","attempts":2}},"accepted_order":1}},"latest_checkpoint":null}}"#;
    let session = DurableSession::open(seeded_store(&[payload]), "restarted-step")
        .await
        .unwrap();
    assert_eq!(
        session
            .state()
            .await
            .unwrap()
            .operation("turn")
            .unwrap()
            .steps["tool-1"]
            .attempts,
        2
    );
}

#[tokio::test]
async fn reopens_a_seeded_step_start() {
    let store = MemoryStore::new().unwrap();
    let session = DurableSession::open(store.clone(), "seeded-step-start")
        .await
        .unwrap();
    session.admit("turn", &"prompt").await.unwrap();
    session.begin_attempt("turn").await.unwrap();
    session
        .begin_step("turn", "tool-1", "tool", &"charge")
        .await
        .unwrap();
    drop(session);
    let session = DurableSession::open(store, "seeded-step-start")
        .await
        .unwrap();

    let state = session.state().await.unwrap();
    let operation = state.operation("turn").unwrap();
    assert!(matches!(
        operation.steps.get("tool-1").map(|step| &step.status),
        Some(nanocodex_durability::StepStatus::EffectPending)
    ));
}

#[tokio::test]
async fn reopens_a_seeded_step_completion() {
    let store = MemoryStore::new().unwrap();
    let session = DurableSession::open(store.clone(), "seeded-step-completion")
        .await
        .unwrap();
    session.admit("turn", &"prompt").await.unwrap();
    session.begin_attempt("turn").await.unwrap();
    session
        .begin_step("turn", "tool-1", "tool", &"charge")
        .await
        .unwrap();
    session
        .complete_step("turn", "tool-1", &"receipt")
        .await
        .unwrap();
    drop(session);
    let session = DurableSession::open(store, "seeded-step-completion")
        .await
        .unwrap();

    let state = session.state().await.unwrap();
    let operation = state.operation("turn").unwrap();
    assert!(matches!(
        operation.steps.get("tool-1").map(|step| &step.status),
        Some(nanocodex_durability::StepStatus::Completed(output))
            if output.decode::<String>().unwrap() == "receipt"
    ));
}

#[tokio::test]
async fn reopens_a_seeded_completion() {
    let store = MemoryStore::new().unwrap();
    let session = DurableSession::open(store.clone(), "seeded-completion")
        .await
        .unwrap();
    session.admit("turn", &"prompt").await.unwrap();
    session.begin_attempt("turn").await.unwrap();
    session
        .complete(
            "turn",
            &Checkpoint { version: 7 },
            &TurnOutput {
                message: "done".to_owned(),
            },
        )
        .await
        .unwrap();
    drop(session);
    let session = DurableSession::open(store, "seeded-completion")
        .await
        .unwrap();

    let replay = session
        .admit_typed::<_, Checkpoint, TurnOutput>("turn", &"prompt")
        .await
        .unwrap();
    assert!(matches!(
        replay,
        Admission::Completed { checkpoint, output }
            if checkpoint == Checkpoint { version: 7 } && output.message == "done"
    ));
}

#[tokio::test]
async fn reopens_a_seeded_terminal_failure() {
    let store = MemoryStore::new().unwrap();
    let session = DurableSession::open(store.clone(), "seeded-terminal-failure")
        .await
        .unwrap();
    session.admit("turn", &"prompt").await.unwrap();
    session.begin_attempt("turn").await.unwrap();
    session
        .fail("turn", &Checkpoint { version: 8 }, "failure")
        .await
        .unwrap();
    drop(session);
    let session = DurableSession::open(store, "seeded-terminal-failure")
        .await
        .unwrap();

    let replay = session
        .admit_typed::<_, Checkpoint, TurnOutput>("turn", &"prompt")
        .await
        .unwrap();
    assert!(matches!(
        replay,
        Admission::Failed { checkpoint, error }
            if checkpoint == Checkpoint { version: 8 } && error == "failure"
    ));
}

#[tokio::test]
async fn queued_unstarted_operation_can_cancel_behind_a_pending_predecessor() {
    let store = MemoryStore::new().unwrap();
    let session = DurableSession::open(store, "queued-cancel").await.unwrap();
    session.admit("turn-1", &"one").await.unwrap();
    session.admit("turn-2", &"two").await.unwrap();
    session.begin_attempt("turn-1").await.unwrap();
    session.cancel("turn-2").await.unwrap();
    assert!(matches!(
        &session
            .state()
            .await
            .unwrap()
            .operation("turn-2")
            .unwrap()
            .status,
        nanocodex_durability::OperationStatus::Cancelled { checkpoint: None }
    ));
    session.complete("turn-1", &1, &"one").await.unwrap();
}

#[tokio::test]
async fn active_operation_cancellation_requires_a_checkpoint() {
    let store = MemoryStore::new().unwrap();
    let session = DurableSession::open(store, "active-cancel-checkpoint")
        .await
        .unwrap();
    session.admit("turn-1", &"one").await.unwrap();
    session.begin_attempt("turn-1").await.unwrap();

    assert!(matches!(
        session.cancel("turn-1").await,
        Err(Error::CancellationCheckpointRequired { operation_id }) if operation_id == "turn-1"
    ));
    assert!(matches!(
        session
            .state()
            .await
            .unwrap()
            .operation("turn-1")
            .unwrap()
            .status,
        nanocodex_durability::OperationStatus::Pending
    ));
}

#[tokio::test]
async fn definitely_uncommitted_terminal_replace_reopens_the_exact_claim_for_retry() {
    let store = MemoryStore::new().unwrap();
    let failed = Arc::new(AtomicBool::new(false));
    let session = DurableSession::open(
        NotCommittedOnceStore {
            inner: store,
            fail_at_revision: 1,
            failed: Arc::clone(&failed),
        },
        "retry-terminal-claim",
    )
    .await
    .unwrap();

    session.admit("turn-1", &"queued").await.unwrap();
    assert!(matches!(
        session.cancel("turn-1").await,
        Err(Error::Store(StoreError::NotCommitted(_)))
    ));
    assert!(failed.load(Ordering::SeqCst));

    assert!(matches!(
        session.admit("turn-1", &"queued").await,
        Ok(Admission::Pending)
    ));
    session.cancel("turn-1").await.unwrap();
    assert!(matches!(
        session.admit("turn-1", &"queued").await,
        Ok(Admission::Cancelled)
    ));
}

#[tokio::test]
async fn definitely_uncommitted_completion_reopens_the_exact_claim_for_retry() {
    let store = MemoryStore::new().unwrap();
    let session = DurableSession::open(
        NotCommittedOnceStore {
            inner: store,
            fail_at_revision: 1,
            failed: Arc::new(AtomicBool::new(false)),
        },
        "retry-completion-claim",
    )
    .await
    .unwrap();
    session.admit("turn-1", &"prompt").await.unwrap();
    session.begin_attempt("turn-1").await.unwrap();
    assert!(matches!(
        session.complete("turn-1", &1, &"answer").await,
        Err(Error::Store(StoreError::NotCommitted(_)))
    ));
    assert!(matches!(
        session.admit("turn-1", &"prompt").await,
        Ok(Admission::Pending)
    ));
    session.begin_attempt("turn-1").await.unwrap();
    session.complete("turn-1", &1, &"answer").await.unwrap();
}

#[tokio::test]
async fn attempt_failure_releases_the_claim_without_a_durable_write() {
    let store = MemoryStore::new().unwrap();
    let session = DurableSession::open(store, "retry-attempt-failure-claim")
        .await
        .unwrap();
    session.admit("turn-1", &"prompt").await.unwrap();
    session.begin_attempt("turn-1").await.unwrap();
    let revision = session.state().await.unwrap().revision();
    session.fail_attempt("turn-1", "temporary").await.unwrap();
    assert_eq!(session.state().await.unwrap().revision(), revision);
    assert!(matches!(
        session.admit("turn-1", &"prompt").await,
        Ok(Admission::Pending)
    ));
    session.begin_attempt("turn-1").await.unwrap();
    session.fail_attempt("turn-1", "temporary").await.unwrap();
    assert!(matches!(
        session.admit("turn-1", &"prompt").await,
        Ok(Admission::Pending)
    ));
}

#[tokio::test]
async fn attempts_require_one_fresh_begin_for_each_claimed_execution() {
    let store = MemoryStore::new().unwrap();
    let session = DurableSession::open(store, "active-attempt-state")
        .await
        .unwrap();
    session.admit("turn-1", &"prompt").await.unwrap();
    session.begin_attempt("turn-1").await.unwrap();
    assert!(matches!(
        session.begin_attempt("turn-1").await,
        Err(Error::AttemptActive { .. })
    ));

    session.fail_attempt("turn-1", "retry").await.unwrap();
    assert!(matches!(
        session.admit("turn-1", &"prompt").await,
        Ok(Admission::Pending)
    ));
    assert!(matches!(
        session.complete("turn-1", &1, &"stale").await,
        Err(Error::AttemptNotStarted { .. })
    ));
    session.begin_attempt("turn-1").await.unwrap();
    session.complete("turn-1", &2, &"fresh").await.unwrap();
}

#[tokio::test]
async fn reclaim_after_release_requires_a_fresh_attempt() {
    let store = MemoryStore::new().unwrap();
    let session = DurableSession::open(store, "released-attempt-state")
        .await
        .unwrap();
    session.admit("turn-1", &"prompt").await.unwrap();
    session.begin_attempt("turn-1").await.unwrap();
    session.release("turn-1").await.unwrap();
    assert!(matches!(
        session.admit("turn-1", &"prompt").await,
        Ok(Admission::Pending)
    ));
    assert!(matches!(
        session.complete("turn-1", &1, &"stale").await,
        Err(Error::AttemptNotStarted { .. })
    ));
    session.begin_attempt("turn-1").await.unwrap();
    session.complete("turn-1", &2, &"fresh").await.unwrap();
}

#[tokio::test]
async fn cloned_handle_cannot_mutate_another_handles_claim() {
    let store = MemoryStore::new().unwrap();
    let owner = DurableSession::open(store, "claim-capability")
        .await
        .unwrap();
    let foreign = owner.clone();
    owner.admit("turn-1", &"prompt").await.unwrap();
    assert!(matches!(
        foreign.begin_attempt("turn-1").await,
        Err(Error::OperationNotClaimed { .. })
    ));
    assert!(matches!(
        foreign.complete("turn-1", &1, &"foreign").await,
        Err(Error::OperationNotClaimed { .. })
    ));
    owner.begin_attempt("turn-1").await.unwrap();
    owner.complete("turn-1", &2, &"owned").await.unwrap();
}

#[tokio::test]
async fn dropping_a_direct_claimant_releases_its_exact_pending_operation() {
    let store = MemoryStore::new().unwrap();
    let root = DurableSession::open(store, "direct-claimant-drop")
        .await
        .unwrap();
    let claimant = root.clone();
    assert!(matches!(
        claimant.admit("turn-1", &"prompt").await,
        Ok(Admission::Accepted)
    ));
    drop(claimant);

    let successor = root.clone();
    assert!(matches!(
        successor.admit("turn-1", &"prompt").await,
        Ok(Admission::Pending)
    ));
    successor.begin_attempt("turn-1").await.unwrap();
    successor
        .complete("turn-1", &1, &"reclaimed")
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clone_drop_churn_and_claim_release_bursts_do_not_starve_commands() {
    let store = MemoryStore::new().unwrap();
    let root = DurableSession::open(store, "clone-drop-liveness")
        .await
        .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let churn = {
        let session = root.clone();
        let stop = Arc::clone(&stop);
        tokio::spawn(async move {
            while !stop.load(Ordering::Acquire) {
                for _ in 0..256 {
                    drop(session.clone());
                }
                tokio::task::yield_now().await;
            }
        })
    };

    let state = root.state();
    tokio::pin!(state);
    tokio::select! {
        outcome = &mut state => {
            outcome.unwrap();
        }
        () = scheduler_budget() => panic!("idle clone/drop churn starved state"),
    }
    let admission = root.admit("live-turn", &"prompt");
    tokio::pin!(admission);
    tokio::select! {
        outcome = &mut admission => {
            assert!(matches!(outcome, Ok(Admission::Accepted)));
        }
        () = scheduler_budget() => panic!("idle clone/drop churn starved admission"),
    }
    stop.store(true, Ordering::Release);
    churn.await.unwrap();
    root.release("live-turn").await.unwrap();

    let mut claimants = Vec::new();
    for index in 0..(RELEASE_BURST_TEST_SIZE) {
        let claimant = root.clone();
        claimant
            .admit(format!("burst-{index}"), &"queued")
            .await
            .unwrap();
        claimants.push(claimant);
    }
    drop(claimants);
    let state = root.state();
    tokio::pin!(state);
    tokio::select! {
        outcome = &mut state => {
            outcome.unwrap();
        }
        () = scheduler_budget() => panic!("claim-release burst starved state"),
    }
}

const RELEASE_BURST_TEST_SIZE: usize = 128;

async fn scheduler_budget() {
    for _ in 0..10_000 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn automatic_admission_reclaims_matching_unclaimed_work() {
    let store = MemoryStore::new().unwrap();
    let session = DurableSession::open(store.clone(), "automatic")
        .await
        .unwrap();
    let first = session
        .admit_automatic_typed::<_, Checkpoint, TurnOutput>(
            "candidate-1",
            &PromptInput {
                prompt: "resume me".to_owned(),
            },
        )
        .await
        .unwrap();
    assert_eq!(first.operation_id(), "candidate-1");
    assert!(matches!(first.into_parts().1, Admission::Accepted));
    drop(session);

    let reopened = DurableSession::open(store, "automatic").await.unwrap();
    let resumed = reopened
        .admit_automatic_typed::<_, Checkpoint, TurnOutput>(
            "candidate-2",
            &PromptInput {
                prompt: "resume me".to_owned(),
            },
        )
        .await
        .unwrap();
    assert_eq!(resumed.operation_id(), "candidate-1");
    assert!(matches!(resumed.into_parts().1, Admission::Pending));
    assert_eq!(reopened.state().await.unwrap().operations().len(), 1);
}

#[tokio::test]
async fn automatic_admission_does_not_guess_past_different_recovered_work() {
    let store = MemoryStore::new().unwrap();
    let session = DurableSession::open(store.clone(), "automatic-blocked")
        .await
        .unwrap();
    session.admit("turn-1", &"first").await.unwrap();
    drop(session);

    let reopened = DurableSession::open(store, "automatic-blocked")
        .await
        .unwrap();
    assert!(matches!(
        reopened
            .admit_automatic_typed::<_, Checkpoint, TurnOutput>("candidate-2", &"different")
            .await,
        Err(Error::OperationBlocked { pending_id, .. }) if pending_id == "turn-1"
    ));
    assert_eq!(reopened.state().await.unwrap().operations().len(), 1);
}

#[tokio::test]
async fn automatic_admission_reclaims_multiple_queued_operations_in_order() {
    let store = MemoryStore::new().unwrap();
    let session = DurableSession::open(store.clone(), "automatic-queue")
        .await
        .unwrap();
    session
        .admit_automatic_typed::<_, Checkpoint, TurnOutput>("turn-1", &"first")
        .await
        .unwrap();
    session
        .admit_automatic_typed::<_, Checkpoint, TurnOutput>("turn-2", &"second")
        .await
        .unwrap();
    drop(session);

    let reopened = DurableSession::open(store, "automatic-queue")
        .await
        .unwrap();
    let first = reopened
        .admit_automatic_typed::<_, Checkpoint, TurnOutput>("new-1", &"first")
        .await
        .unwrap();
    let second = reopened
        .admit_automatic_typed::<_, Checkpoint, TurnOutput>("new-2", &"second")
        .await
        .unwrap();
    assert_eq!(first.operation_id(), "turn-1");
    assert_eq!(second.operation_id(), "turn-2");
    assert!(matches!(first.into_parts().1, Admission::Pending));
    assert!(matches!(second.into_parts().1, Admission::Pending));
    assert_eq!(reopened.state().await.unwrap().operations().len(), 2);
}

#[tokio::test]
async fn rejects_invalid_transitions_before_the_host_replace() {
    let store = MemoryStore::new().unwrap();
    let session = DurableSession::open(store.clone(), "session")
        .await
        .unwrap();
    assert!(matches!(
        session.cancel("missing-operation").await,
        Err(Error::OperationNotClaimed { .. })
    ));
    assert_eq!(session.state().await.unwrap().revision(), 0);

    drop(session);
    let reopened = DurableSession::open(store, "session").await.unwrap();
    assert_eq!(reopened.state().await.unwrap().revision(), 0);
}

#[tokio::test]
async fn stops_a_stale_owner_when_a_replace_outcome_is_ambiguous() {
    let store = MemoryStore::new().unwrap();
    let session = DurableSession::open(
        CommitThenFailStore {
            inner: store.clone(),
            fail_after_revision: 1,
        },
        "session",
    )
    .await
    .unwrap();
    session.admit("turn-1", &"hello").await.unwrap();
    session.begin_attempt("turn-1").await.unwrap();
    assert!(matches!(
        session.complete("turn-1", &1, &"done").await,
        Err(Error::Store(StoreError::Backend(_)))
    ));
    assert!(matches!(session.state().await, Err(Error::DriverStopped)));

    let reopened = DurableSession::open(store, "session").await.unwrap();
    assert!(matches!(
        reopened.admit("turn-1", &"hello").await,
        Ok(Admission::Completed { .. })
    ));
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn sqlite_compare_and_replace_survives_reopen() {
    use nanocodex_durability::SqliteStore;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("durability.sqlite3");
    let session = DurableSession::open(SqliteStore::open(&path).unwrap(), "session")
        .await
        .unwrap();
    session.admit("turn-1", &"hello").await.unwrap();
    session.begin_attempt("turn-1").await.unwrap();
    session
        .complete("turn-1", &Checkpoint { version: 1 }, &"done")
        .await
        .unwrap();
    drop(session);

    let reopened = DurableSession::open(SqliteStore::open(path).unwrap(), "session")
        .await
        .unwrap();
    assert!(matches!(
        reopened.admit("turn-1", &"hello").await,
        Ok(Admission::Completed { .. })
    ));
}

#[tokio::test]
async fn large_checkpoint_reopens_with_exact_receipts_and_pending_step_replay() {
    let store = MemoryStore::new().unwrap();
    let session = DurableSession::open(store.clone(), "large-checkpoint")
        .await
        .unwrap();
    let checkpoint = "quoted \" unicode 🧪\n".repeat(50_000);
    session.admit("completed", &"input").await.unwrap();
    session.begin_attempt("completed").await.unwrap();
    session
        .complete("completed", &checkpoint, &"result")
        .await
        .unwrap();
    session.admit("pending", &"next").await.unwrap();
    session.begin_attempt("pending").await.unwrap();
    session
        .begin_step("pending", "tool", "tool", &"effect")
        .await
        .unwrap();
    session
        .complete_step("pending", "tool", &checkpoint)
        .await
        .unwrap();
    drop(session);

    let reopened = DurableSession::open(store, "large-checkpoint")
        .await
        .unwrap();
    assert_eq!(
        reopened
            .latest_checkpoint()
            .await
            .unwrap()
            .unwrap()
            .decode::<String>()
            .unwrap(),
        checkpoint
    );
    match reopened
        .admit_typed::<_, String, String>("completed", &"input")
        .await
        .unwrap()
    {
        Admission::Completed {
            checkpoint: replay,
            output,
        } => {
            assert_eq!(replay, checkpoint);
            assert_eq!(output, "result");
        }
        other => panic!("expected exact completed replay, got {other:?}"),
    }
    assert!(matches!(
        reopened.admit("pending", &"next").await.unwrap(),
        Admission::Pending
    ));
    reopened.begin_attempt("pending").await.unwrap();
    match reopened
        .begin_step("pending", "tool", "tool", &"effect")
        .await
        .unwrap()
    {
        BeginStep::Replay(output) => assert_eq!(output.decode::<String>().unwrap(), checkpoint),
        other => panic!("expected committed tool replay, got {other:?}"),
    }
    reopened
        .complete("pending", &checkpoint, &"continued")
        .await
        .unwrap();
}
