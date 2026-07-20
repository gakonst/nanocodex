use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

use super::{
    BlockRequest, FileEdit, FileMutation, LineAnchor, PatchRequest, ReadRequest, TransactionAction,
    TransactionRequest, exact_digest, execute_patch, execute_transaction, find_block, hash_hex,
    line_hash, read,
};

fn workspace(name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "nanocodex-hashline-{name}-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir(&path).expect("temporary workspace should be created");
    path
}

#[test]
fn canonical_hash_fixtures_cover_normalization_and_exact_bytes() {
    assert_eq!(line_hash("alpha"), "93c8");
    assert_eq!(hash_hex("alpha\r\nbeta\r\n"), hash_hex("alpha\nbeta\n"));
    assert_eq!(hash_hex("\u{feff}alpha\n"), hash_hex("alpha\n"));
    assert_ne!(
        exact_digest("alpha\r\n".as_bytes()),
        exact_digest("alpha\n".as_bytes())
    );
    assert_eq!(
        exact_digest(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn read_anchors_feed_patch_and_stale_guards_fail_without_writing() {
    let root = workspace("read-patch");
    fs::write(root.join("notes.txt"), b"alpha\r\nbeta\r\n").expect("fixture should write");
    let result = read(
        &root,
        &ReadRequest {
            path: "notes.txt".to_owned(),
            start_line: None,
            end_line: None,
            max_lines: None,
        },
    )
    .expect("read should succeed");
    assert_eq!(result["lines"][1]["hash"], "f589");
    assert_eq!(result["content"], "1:93c8|alpha\n2:f589|beta");
    let patch = format!(
        "{}\nSWAP 2:{}:\n+bravo",
        result["header"].as_str().expect("header should be text"),
        result["lines"][1]["hash"]
            .as_str()
            .expect("hash should be text")
    );
    execute_patch(
        &root,
        &PatchRequest {
            path: "notes.txt".to_owned(),
            patch: patch.clone(),
            dry_run: false,
            create: false,
        },
    )
    .expect("anchored patch should succeed");
    assert_eq!(
        fs::read(root.join("notes.txt")).expect("file should read"),
        b"alpha\r\nbravo\r\n"
    );
    let error = execute_patch(
        &root,
        &PatchRequest {
            path: "notes.txt".to_owned(),
            patch,
            dry_run: false,
            create: false,
        },
    )
    .expect_err("stale patch should fail");
    assert!(error.to_string().contains("hash mismatch"));
    assert_eq!(
        fs::read(root.join("notes.txt")).expect("file should read"),
        b"alpha\r\nbravo\r\n"
    );
    fs::remove_dir_all(root).expect("workspace should be removed");
}

#[test]
fn block_anchor_round_trips_and_rejects_stale_evidence() {
    let root = workspace("block");
    fs::write(root.join("lib.rs"), "fn one() {\n    work();\n}\n").expect("fixture should write");
    let anchor = format!("2:{}", line_hash("    work();"));
    let found = find_block(
        &root,
        &BlockRequest {
            path: "lib.rs".to_owned(),
            anchor,
            max_lines: None,
        },
    )
    .expect("block should resolve");
    assert_eq!(found["start_line"], 1);
    let combined = found["block_anchor"]
        .as_str()
        .expect("block anchor should be text");
    find_block(
        &root,
        &BlockRequest {
            path: "lib.rs".to_owned(),
            anchor: combined.to_owned(),
            max_lines: None,
        },
    )
    .expect("combined anchor should round trip");
    fs::write(root.join("lib.rs"), "fn one() {\n    changed();\n}\n")
        .expect("fixture should change");
    assert!(
        find_block(
            &root,
            &BlockRequest {
                path: "lib.rs".to_owned(),
                anchor: combined.to_owned(),
                max_lines: None,
            }
        )
        .expect_err("stale anchor should fail")
        .to_string()
        .contains("mismatch")
    );
    fs::remove_dir_all(root).expect("workspace should be removed");
}

#[test]
fn transaction_preview_digest_commits_exact_plan_and_cleans_sidecar() {
    let root = workspace("transaction");
    let before = b"alpha\nbeta\n";
    fs::write(root.join("a.txt"), before).expect("fixture should write");
    let mutations = vec![FileMutation::Update {
        path: "a.txt".to_owned(),
        expected: super::ExpectedFile {
            exact_digest: exact_digest(before),
        },
        edits: vec![FileEdit::InsertAfter {
            anchor: LineAnchor {
                line: 1,
                expected_hash: line_hash("alpha"),
            },
            lines: vec!["inserted".to_owned()],
        }],
    }];
    let preview = execute_transaction(
        &root,
        &TransactionRequest {
            action: TransactionAction::Preview,
            root: None,
            mutations,
        },
    )
    .expect("preview should succeed");
    let digest = preview["planDigest"]
        .as_str()
        .expect("digest should be text")
        .to_owned();
    assert_eq!(digest.len(), 64);
    assert_eq!(
        fs::read(root.join("a.txt")).expect("file should read"),
        before
    );

    let result = execute_transaction(
        &root,
        &serde_json::from_value(json!({
            "action": {"type": "commitPreviewed", "expectedPlanDigest": digest},
            "mutations": [{
                "type": "update",
                "path": "a.txt",
                "expected": {"exactDigest": exact_digest(before)},
                "edits": [{"type": "insertAfter", "anchor": {"line": 1, "expectedHash": line_hash("alpha")}, "lines": ["inserted"]}]
            }]
        }))
        .expect("request should decode"),
    )
    .expect("previewed commit should succeed");
    assert_eq!(result["outcome"], "committed");
    assert_eq!(
        fs::read(root.join("a.txt")).expect("file should read"),
        b"alpha\ninserted\nbeta\n"
    );
    assert!(!root.join(".nanocodex/hashline-transactions").exists());
    fs::remove_dir_all(root).expect("workspace should be removed");
}

#[test]
fn pending_journal_recovers_all_before_state_in_a_fresh_invocation() {
    let root = workspace("recovery");
    fs::write(root.join("a.txt"), "before-a\n").expect("fixture should write");
    fs::write(root.join("b.txt"), "before-b\n").expect("fixture should write");
    let prepared = vec![
        super::PreparedMutation::Write {
            path: "a.txt".to_owned(),
            before: Some(b"before-a\n".to_vec()),
            after: b"after-a\n".to_vec(),
        },
        super::PreparedMutation::Write {
            path: "b.txt".to_owned(),
            before: Some(b"before-b\n".to_vec()),
            after: b"after-b\n".to_vec(),
        },
    ];
    super::write_journal(&root, "recovery-fixture", &prepared).expect("journal should be durable");
    super::apply_one(&root, &prepared[0]).expect("first mutation should apply");

    super::recover_pending(&root).expect("fresh recovery should converge");

    assert_eq!(
        fs::read(root.join("a.txt")).expect("file should read"),
        b"before-a\n"
    );
    assert_eq!(
        fs::read(root.join("b.txt")).expect("file should read"),
        b"before-b\n"
    );
    assert!(!root.join(".nanocodex/hashline-transactions").exists());
    fs::remove_dir_all(root).expect("workspace should be removed");
}
