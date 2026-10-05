//! Public owned-store acceptance: real SQLite, no private journal writes.
#![cfg(feature = "sqlite")]
use nanocodex_durability::{CheckpointBranch, DurableSession, SqliteStore};
use serde_json::{Value, json};

#[tokio::test]
async fn settled_branch_preserves_parent_and_rejects_pending_or_superseded_source() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let open = || SqliteStore::open(&path).unwrap();
    let parent = DurableSession::open(open(), "parent").await.unwrap();
    parent
        .admit("first", &json!({"prompt":"first"}))
        .await
        .unwrap();
    parent.begin_attempt("first").await.unwrap();
    parent
        .complete("first", &json!({"messages":["first"]}), &json!("one"))
        .await
        .unwrap();
    parent
        .admit("second", &json!({"prompt":"second"}))
        .await
        .unwrap();
    parent.begin_attempt("second").await.unwrap();
    parent
        .complete(
            "second",
            &json!({"messages":["first","second"]}),
            &json!("two"),
        )
        .await
        .unwrap();
    drop(parent);
    let mut source = CheckpointBranch::open(open(), "parent").await.unwrap();
    assert!(source.before("first").await.unwrap().is_none());
    let before: Value = source
        .before("second")
        .await
        .unwrap()
        .unwrap()
        .decode()
        .unwrap();
    assert_eq!(before, json!({"messages":["first"]}));
    let branch_id = source.publish(&before).await.unwrap();
    assert_ne!(branch_id, "parent");
    assert!(uuid::Uuid::parse_str(&branch_id).is_ok());
    let branch = DurableSession::open(open(), &branch_id).await.unwrap();
    assert!(branch.state().await.unwrap().operations().is_empty());
    assert_eq!(
        branch
            .latest_checkpoint()
            .await
            .unwrap()
            .unwrap()
            .decode::<Value>()
            .unwrap(),
        before
    );
    drop(branch);
    let original = DurableSession::open(open(), "parent").await.unwrap();
    assert_eq!(
        original
            .latest_checkpoint()
            .await
            .unwrap()
            .unwrap()
            .decode::<Value>()
            .unwrap(),
        json!({"messages":["first","second"]})
    );
    original
        .admit("continued", &json!({"prompt":"still usable"}))
        .await
        .unwrap();
    original.begin_attempt("continued").await.unwrap();
    original
        .complete(
            "continued",
            &json!({"messages":["continued"]}),
            &json!("three"),
        )
        .await
        .unwrap();
    drop(original);
    let stale = CheckpointBranch::open(open(), "parent").await.unwrap();
    let superseding = DurableSession::open(open(), "parent").await.unwrap();
    let error = stale.publish(&before).await.unwrap_err().to_string();
    assert!(error.contains("fenced"), "{error}");
    superseding
        .admit("pending", &json!({"prompt":"unfinished"}))
        .await
        .unwrap();
    drop(superseding);
    let error = match CheckpointBranch::open(open(), "parent").await {
        Ok(_) => panic!("pending source accepted"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("pending operations"), "{error}");
    eprintln!(
        "real SQLite public API: before-turn selection, fresh UUID, empty execution queue, original continued, superseded owner rejected, pending operation rejected"
    );
}
