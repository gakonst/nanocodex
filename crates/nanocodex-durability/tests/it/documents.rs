//! Public session API journey, run in separate OS processes against a real SQLite database.
#![cfg(feature = "sqlite")]
use nanocodex_durability::{
    Admission, BeginStep, DocumentForkPolicy as Policy, DocumentWrite, DurableSession, Error,
    OwnedState, OwnerId, OwnerToken, SqliteStore, StateStore, StoreError, StoreFuture, StoreRecord,
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

fn write(key: &str, version: u64, value: Value, fork: Policy) -> DocumentWrite {
    DocumentWrite {
        key: key.into(),
        expected_version: version,
        value,
        fork,
    }
}
async fn start(session: &DurableSession, id: &str) {
    assert!(matches!(
        session.admit(id, &id).await.unwrap(),
        Admission::Accepted | Admission::Pending
    ));
    session.begin_attempt(id).await.unwrap();
}

struct RejectOnce {
    inner: SqliteStore,
    reject: Arc<AtomicBool>,
}
impl StateStore for RejectOnce {
    fn read_record<'a>(
        &'a mut self,
        id: &'a str,
        key: &'a str,
    ) -> StoreFuture<'a, Result<Option<String>, StoreError>> {
        self.inner.read_record(id, key)
    }
    fn acquire<'a>(
        &'a mut self,
        id: &'a str,
        owner: OwnerId,
    ) -> StoreFuture<'a, Result<OwnedState, StoreError>> {
        self.inner.acquire(id, owner)
    }
    fn replace<'a>(
        &'a mut self,
        id: &'a str,
        owner: &'a OwnerToken,
        revision: u64,
        payload: &'a str,
        records: &'a [StoreRecord],
    ) -> StoreFuture<'a, Result<u64, StoreError>> {
        if self.reject.swap(false, Ordering::SeqCst) {
            Box::pin(async {
                Err(StoreError::NotCommitted(
                    "injected SQLite transaction rejection".into(),
                ))
            })
        } else {
            self.inner.replace(id, owner, revision, payload, records)
        }
    }
}

#[test]
fn session_documents_survive_process_restart_and_fork_atomically() {
    let directory = tempfile::tempdir().unwrap();
    for stage in ["seed", "restart-and-fork", "verify-branches"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "documents::process_journey",
                "--ignored",
                "--nocapture",
            ])
            .env(
                "NANOCODEX_DOCUMENT_JOURNEY_DB",
                directory.path().join("documents.sqlite"),
            )
            .env("NANOCODEX_DOCUMENT_JOURNEY_STAGE", stage)
            .output()
            .unwrap();
        print!("stage={stage}\n{}", String::from_utf8_lossy(&output.stdout));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[tokio::test]
#[ignore = "subprocess fixture driven by public API journey"]
async fn process_journey() {
    let db = std::env::var("NANOCODEX_DOCUMENT_JOURNEY_DB").unwrap();
    let stage = std::env::var("NANOCODEX_DOCUMENT_JOURNEY_STAGE").unwrap();
    let reject = Arc::new(AtomicBool::new(false));
    let source = DurableSession::open(
        RejectOnce {
            inner: SqliteStore::open(&db).unwrap(),
            reject: reject.clone(),
        },
        "source",
    )
    .await
    .unwrap();
    if stage == "seed" {
        start(&source, "early").await;
        source
            .complete_with_documents(
                "early",
                &json!({"cursor": 1}),
                &json!({"receipt": "early-result"}),
                vec![
                    write("initial", 0, json!(1), Policy::Initial),
                    write("current", 0, json!(1), Policy::Current),
                    write("asof", 0, json!(1), Policy::AsOf),
                ],
            )
            .await
            .unwrap();
        start(&source, "later").await;
        assert!(matches!(
            source
                .begin_step(
                    "later",
                    "effect",
                    "fixture",
                    &json!({"key":"provider-stable-id"})
                )
                .await
                .unwrap(),
            BeginStep::Execute
        ));
        source
            .complete_step_with_documents(
                "later",
                "effect",
                &json!({"providerReceipt":"ok"}),
                vec![write("current", 1, json!(2), Policy::Current)],
            )
            .await
            .unwrap();
        let revision = source.state().await.unwrap().revision();
        // A later conflict must roll back even a preceding valid write and the receipt.
        let invalid = source
            .complete_with_documents(
                "later",
                &json!({"cursor":2}),
                &"ok",
                vec![
                    write("initial", 1, json!(2), Policy::Initial),
                    write("asof", 99, json!(2), Policy::AsOf),
                ],
            )
            .await;
        assert!(matches!(invalid, Err(Error::InvalidState(_))));
        assert_eq!(source.state().await.unwrap().revision(), revision);
        assert_eq!(
            source.document("initial").await.unwrap().unwrap().value,
            json!(1)
        );
        reject.store(true, Ordering::SeqCst);
        let rejected = source
            .complete_with_documents(
                "later",
                &json!({"cursor":2}),
                &"ok",
                vec![write("initial", 1, json!(2), Policy::Initial)],
            )
            .await;
        assert!(matches!(
            rejected,
            Err(Error::Store(StoreError::NotCommitted(_)))
        ));
        assert_eq!(
            source.document("initial").await.unwrap().unwrap().value,
            json!(1)
        );
        start(&source, "later").await;
        assert!(
            matches!(source.begin_step_typed::<_,Value>("later","effect","fixture", &json!({"key":"provider-stable-id"})).await.unwrap(),BeginStep::Replay(value) if value["providerReceipt"] == "ok")
        );
        source
            .complete_with_documents(
                "later",
                &json!({"cursor":2}),
                &json!({"receipt":"later-result"}),
                vec![
                    write("initial", 1, json!(2), Policy::Initial),
                    write("asof", 1, json!(2), Policy::AsOf),
                    write("late", 0, json!("late"), Policy::AsOf),
                ],
            )
            .await
            .unwrap();
        println!(
            "atomic conflict and rejected transaction preserved documents; effect receipt replayed; seeded early/later checkpoints"
        );
    } else if stage == "restart-and-fork" {
        let receipt: Admission<Value, Value> = source.admit_typed("later", &"later").await.unwrap();
        assert!(
            matches!(receipt,Admission::Completed {checkpoint,output} if checkpoint["cursor"] == 2 && output["receipt"] == "later-result")
        );
        assert_eq!(
            source.document("current").await.unwrap().unwrap().value,
            json!(2)
        );
        for destination in ["branch-a", "branch-b"] {
            let (checkpoint, fork) = source.document_fork("early").await.unwrap();
            assert_eq!(checkpoint.decode::<Value>().unwrap(), json!({"cursor":1}));
            let branch = DurableSession::open(SqliteStore::open(&db).unwrap(), destination)
                .await
                .unwrap();
            branch
                .initialize_document_fork(fork, &checkpoint)
                .await
                .unwrap();
            assert_eq!(
                branch
                    .latest_checkpoint()
                    .await
                    .unwrap()
                    .unwrap()
                    .decode::<Value>()
                    .unwrap(),
                json!({"cursor":1})
            );
            assert_eq!(
                branch.document("initial").await.unwrap().unwrap().value,
                json!(1)
            );
            assert_eq!(
                branch.document("current").await.unwrap().unwrap().value,
                json!(2)
            );
            assert_eq!(
                branch.document("asof").await.unwrap().unwrap().value,
                json!(1)
            );
            assert!(branch.document("late").await.unwrap().is_none());
            if destination == "branch-a" {
                start(&branch, "change").await;
                branch
                    .complete_with_documents(
                        "change",
                        &json!({"cursor":3}),
                        &"changed",
                        vec![write("asof", 1, json!(42), Policy::AsOf)],
                    )
                    .await
                    .unwrap();
            }
            let (_, seed) = source.document_fork("early").await.unwrap();
            assert!(
                branch
                    .initialize_document_fork(seed, &checkpoint)
                    .await
                    .is_err()
            );
        }
        start(&source, "block").await;
        source
            .complete_with_documents(
                "block",
                &json!({"cursor":3}),
                &"blocked",
                vec![write("lock", 0, json!(true), Policy::Block)],
            )
            .await
            .unwrap();
        assert!(source.document_fork("early").await.is_err());
        println!(
            "cold restart replayed checkpoint+receipt+documents; historical policies initial=1/current=2/asof=1; late key omitted; occupied destination and block policy rejected"
        );
    } else {
        for (destination, expected) in [("source", 2), ("branch-a", 42), ("branch-b", 1)] {
            let branch = DurableSession::open(SqliteStore::open(&db).unwrap(), destination)
                .await
                .unwrap();
            assert_eq!(
                branch.document("asof").await.unwrap().unwrap().value,
                json!(expected)
            );
        }
        println!("third process verified independent persisted source=2 branch-a=42 branch-b=1");
    }
}
