//! Content-defined payload chunks and the owner's committed-record cache must
//! never let a head reference a record that did not commit, across precommit
//! failures, lost acknowledgements and reopen.
use nanocodex_durability::{
    BeginStep, DurableSession, MemoryStore, OwnedState, OwnerId, OwnerToken, StateStore,
    StepStatus, StoreError, StoreFuture, StoreRecord,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, PartialEq)]
enum Fault {
    None,
    NotCommitted,
    LostAck,
}

#[derive(Default)]
struct Log {
    /// Records offered by each replace attempt.
    offered: Vec<Vec<String>>,
    /// Bytes offered by each replace attempt.
    bytes: Vec<usize>,
}

struct Faulty {
    inner: MemoryStore,
    next: Arc<Mutex<Fault>>,
    log: Arc<Mutex<Log>>,
}

impl StateStore for Faulty {
    fn read_record<'a>(
        &'a mut self,
        state_id: &'a str,
        key: &'a str,
    ) -> StoreFuture<'a, Result<Option<String>, StoreError>> {
        self.inner.read_record(state_id, key)
    }
    fn acquire<'a>(
        &'a mut self,
        state_id: &'a str,
        owner: OwnerId,
    ) -> StoreFuture<'a, Result<OwnedState, StoreError>> {
        self.inner.acquire(state_id, owner)
    }
    fn replace<'a>(
        &'a mut self,
        state_id: &'a str,
        owner: &'a OwnerToken,
        revision: u64,
        payload: &'a str,
        records: &'a [StoreRecord],
    ) -> StoreFuture<'a, Result<u64, StoreError>> {
        Box::pin(async move {
            {
                let mut log = self.log.lock().unwrap();
                log.offered
                    .push(records.iter().map(|record| record.key.clone()).collect());
                log.bytes
                    .push(records.iter().map(|record| record.value.len()).sum());
            }
            let fault = std::mem::replace(&mut *self.next.lock().unwrap(), Fault::None);
            match fault {
                Fault::NotCommitted => Err(StoreError::NotCommitted("synthetic".into())),
                Fault::LostAck => {
                    self.inner
                        .replace(state_id, owner, revision, payload, records)
                        .await?;
                    Err(StoreError::Backend("synthetic lost acknowledgement".into()))
                }
                Fault::None => {
                    self.inner
                        .replace(state_id, owner, revision, payload, records)
                        .await
                }
            }
        })
    }
}

fn transcript(messages: usize) -> Value {
    let messages: Vec<_> = (0..messages)
        .map(|index| {
            let body: String = (0..120)
                .map(|line| {
                    format!(
                        "message {index} line {line}: {:x}\n",
                        index * 7919 + line * 104_729
                    )
                })
                .collect();
            json!({"role": if index % 2 == 0 { "user" } else { "assistant" }, "content": body})
        })
        .collect();
    json!({"subscription_wire_v1": serde_json::to_string(&json!({"messages": messages, "tools": "static"})).unwrap()})
}

async fn open(
    store: &MemoryStore,
    next: &Arc<Mutex<Fault>>,
    log: &Arc<Mutex<Log>>,
) -> DurableSession {
    DurableSession::open(
        Faulty {
            inner: store.clone(),
            next: next.clone(),
            log: log.clone(),
        },
        "delta-journal",
    )
    .await
    .unwrap()
}

async fn step_input(session: &DurableSession, step: &str) -> String {
    let state = session.state().await.unwrap();
    let input = state.operation("turn").unwrap().steps[step].input.clone();
    session
        .resolve(&input)
        .await
        .unwrap()
        .json()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn chunked_requests_survive_precommit_failure_lost_ack_and_reopen() {
    let store = MemoryStore::new().unwrap();
    let next = Arc::new(Mutex::new(Fault::None));
    let log = Arc::new(Mutex::new(Log::default()));
    let session = open(&store, &next, &log).await;
    session.admit("turn", &"prompt").await.unwrap();
    session.begin_attempt("turn").await.unwrap();

    // A precommit failure must not mark its chunks committed: the retry in
    // the same owner offers every chunk again.
    let first = transcript(40);
    *next.lock().unwrap() = Fault::NotCommitted;
    assert!(
        session
            .begin_step("turn", "model-1", "model_call", &first)
            .await
            .is_err()
    );
    assert!(matches!(
        session
            .begin_step("turn", "model-1", "model_call", &first)
            .await
            .unwrap(),
        BeginStep::Execute
    ));
    {
        let log = log.lock().unwrap();
        let attempts = &log.offered[log.offered.len() - 2..];
        let chunks = |keys: &Vec<String>| keys.iter().filter(|key| key.starts_with("c:")).count();
        assert!(
            chunks(&attempts[0]) > 4,
            "a large request is content-chunked"
        );
        assert_eq!(chunks(&attempts[0]), chunks(&attempts[1]));
    }
    session
        .complete_step("turn", "model-1", &"response-1")
        .await
        .unwrap();

    // The next, longer request offers only new chunks plus its manifest.
    let second = transcript(42);
    let size = serde_json::to_string(&second).unwrap().len();
    assert!(matches!(
        session
            .begin_step("turn", "model-2", "model_call", &second)
            .await
            .unwrap(),
        BeginStep::Execute
    ));
    let offered = *log.lock().unwrap().bytes.last().unwrap();
    assert!(offered * 4 < size, "offered {offered} of {size} bytes");
    assert_eq!(
        step_input(&session, "model-2").await,
        serde_json::to_string(&second).unwrap()
    );

    // A lost acknowledgement poisons the owner; a reopened owner replays the
    // committed step with the identical frozen request and fresh cache.
    let third = transcript(44);
    session
        .complete_step("turn", "model-2", &"response-2")
        .await
        .unwrap();
    *next.lock().unwrap() = Fault::LostAck;
    assert!(
        session
            .begin_step("turn", "model-3", "model_call", &third)
            .await
            .is_err()
    );
    drop(session);

    let reopened = open(&store, &next, &log).await;
    reopened.admit("turn", &"prompt").await.unwrap();
    reopened.begin_attempt("turn").await.unwrap();
    let state = reopened.state().await.unwrap();
    assert!(matches!(
        state.operation("turn").unwrap().steps["model-3"].status,
        StepStatus::EffectPending
    ));
    assert_eq!(
        step_input(&reopened, "model-3").await,
        serde_json::to_string(&third).unwrap()
    );
    // Same frozen request: same payload identity, so the step re-executes
    // rather than conflicting; a different request is rejected.
    assert!(matches!(
        reopened
            .begin_step("turn", "model-3", "model_call", &third)
            .await
            .unwrap(),
        BeginStep::Execute
    ));
    assert!(
        reopened
            .begin_step("turn", "model-3", "model_call", &transcript(45))
            .await
            .is_err()
    );
    reopened
        .complete_step("turn", "model-3", &"response-3")
        .await
        .unwrap();
    assert!(matches!(
        reopened
            .begin_step("turn", "model-3", "model_call", &third)
            .await
            .unwrap(),
        BeginStep::Replay(_)
    ));
}
