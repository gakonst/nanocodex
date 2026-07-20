use std::fs;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;
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
    assert!(result.get("lines").is_none());
    assert_eq!(result["content"], "1:93c8|alpha\n2:f589|beta");
    let patch = format!(
        "{}\nSWAP 2:{}:\n+bravo",
        result["header"].as_str().expect("header should be text"),
        line_hash("beta")
    );
    execute_patch(
        &root,
        &PatchRequest {
            patch: patch.clone(),
            dry_run: false,
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
            patch,
            dry_run: false,
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
fn tool_schemas_explain_relative_paths_and_patch_grammar() {
    let read =
        serde_json::to_value(super::read_definition()).expect("read definition should serialize");
    assert!(
        read["parameters"]["properties"]["path"]["description"]
            .as_str()
            .expect("read path description should be text")
            .contains("workspace-relative")
    );

    let patch =
        serde_json::to_value(super::patch_definition()).expect("patch definition should serialize");
    let patch_description = patch["parameters"]["properties"]["patch"]["description"]
        .as_str()
        .expect("patch description should be text");
    assert_eq!(
        patch["parameters"]["required"],
        json!(["patch"]),
        "patch schema should require only the complete program"
    );
    let properties = patch["parameters"]["properties"]
        .as_object()
        .expect("patch properties should be an object");
    assert_eq!(properties.len(), 2);
    assert!(properties.contains_key("patch"));
    assert!(properties.contains_key("dry_run"));
    for required in [
        "[path]#HASH",
        "[new.txt]",
        "SWAP 2:f589",
        "+replacement",
        "REM",
        "MV",
        "workspace-relative",
    ] {
        assert!(
            patch_description.contains(required),
            "patch description should document {required}"
        );
    }

    let transaction = serde_json::to_value(super::transaction_definition())
        .expect("transaction definition should serialize");
    assert!(
        transaction["parameters"]["properties"]["root"]["description"]
            .as_str()
            .expect("transaction root description should be text")
            .contains("Omit or use \".\"")
    );
    let transaction_description = transaction["description"]
        .as_str()
        .expect("transaction description should be text");
    for required in [
        "commitPreviewed",
        "identical mutations",
        "expectedPlanDigest",
    ] {
        assert!(transaction_description.contains(required));
    }
    for mutation in transaction["parameters"]["properties"]["mutations"]["items"]["oneOf"]
        .as_array()
        .expect("mutation variants should be an array")
    {
        let path_schema = mutation["properties"]
            .get("path")
            .or_else(|| mutation["properties"].get("source"))
            .expect("mutation should expose a path or source");
        assert!(
            path_schema["description"]
                .as_str()
                .expect("mutation path description should be text")
                .contains("relative to the selected transaction root")
        );
    }
}

#[test]
fn one_patch_mixes_create_update_move_and_delete_sections() {
    let root = workspace("mixed-patch");
    fs::write(root.join("update.txt"), b"old\n").expect("update fixture should write");
    fs::write(root.join("move.txt"), b"move\n").expect("move fixture should write");
    fs::write(root.join("delete.txt"), b"delete\n").expect("delete fixture should write");
    let program = format!(
        "[update.txt]#{}\nSWAP 1:{}:\n+new\n[new.txt]\nINS.HEAD:\n+created\n[move.txt]#{}\nMV moved.txt\n[delete.txt]#{}\nREM",
        hash_hex("old\n"),
        line_hash("old"),
        hash_hex("move\n"),
        hash_hex("delete\n")
    );
    execute_patch(
        &root,
        &PatchRequest {
            patch: program,
            dry_run: false,
        },
    )
    .expect("mixed patch should commit");
    assert_eq!(
        fs::read(root.join("update.txt")).expect("update should read"),
        b"new\n"
    );
    assert_eq!(
        fs::read(root.join("new.txt")).expect("create should read"),
        b"created"
    );
    assert_eq!(
        fs::read(root.join("moved.txt")).expect("move should read"),
        b"move\n"
    );
    assert!(!root.join("move.txt").exists());
    assert!(!root.join("delete.txt").exists());

    let existing_create = execute_patch(
        &root,
        &PatchRequest {
            patch: "[new.txt]\nINS.TAIL:\n+again".to_owned(),
            dry_run: true,
        },
    )
    .expect_err("[path] creation should reject an existing target")
    .to_string();
    assert!(existing_create.contains("already exists"));
    let guarded_missing = execute_patch(
        &root,
        &PatchRequest {
            patch: "[missing.txt]#0123abcd\nREM".to_owned(),
            dry_run: true,
        },
    )
    .expect_err("[path]#HASH should reject a missing target")
    .to_string();
    assert!(guarded_missing.contains("missing.txt"));
    fs::remove_dir_all(root).expect("workspace should be removed");
}

#[test]
fn wrong_dialect_and_format_staleness_never_write() {
    let root = workspace("dialect-format");
    fs::write(root.join("notes.txt"), b"alpha\nbeta\nomega\n").expect("fixture should write");
    let before = fs::read(root.join("notes.txt")).expect("fixture should read");
    let dialect = execute_patch(
        &root,
        &PatchRequest {
            patch: "*** Begin Patch\n*** Update File: notes.txt\n@@\n*** End Patch".to_owned(),
            dry_run: false,
        },
    )
    .expect_err("apply_patch dialect should fail")
    .to_string();
    assert!(dialect.contains("line 1"));
    assert!(dialect.contains("Hashline"));
    assert_eq!(
        fs::read(root.join("notes.txt")).expect("fixture should read"),
        before
    );

    let initial = read(
        &root,
        &ReadRequest {
            path: "notes.txt".to_owned(),
            start_line: None,
            end_line: None,
            max_lines: None,
        },
    )
    .expect("initial read should succeed");
    let stale_program = format!(
        "{}\nSWAP 2:{}:\n+bravo",
        initial["header"].as_str().expect("header should be text"),
        line_hash("beta")
    );
    fs::write(root.join("notes.txt"), b"ALPHA\nbeta\nomega\n")
        .expect("formatter fixture should write");
    execute_patch(
        &root,
        &PatchRequest {
            patch: stale_program,
            dry_run: false,
        },
    )
    .expect_err("formatting should stale prior evidence");
    assert_eq!(
        fs::read(root.join("notes.txt")).expect("formatted fixture should read"),
        b"ALPHA\nbeta\nomega\n"
    );
    let fresh = read(
        &root,
        &ReadRequest {
            path: "notes.txt".to_owned(),
            start_line: Some(1),
            end_line: Some(3),
            max_lines: Some(3),
        },
    )
    .expect("bounded reread should succeed");
    execute_patch(
        &root,
        &PatchRequest {
            patch: format!(
                "{}\nSWAP 2:{}:\n+bravo",
                fresh["header"].as_str().expect("header should be text"),
                line_hash("beta")
            ),
            dry_run: false,
        },
    )
    .expect("rebuilt patch should succeed");
    assert_eq!(
        fs::read(root.join("notes.txt")).expect("result should read"),
        b"ALPHA\nbravo\nomega\n"
    );
    fs::remove_dir_all(root).expect("workspace should be removed");
}

#[test]
fn patch_rejections_explain_a_valid_hashline_retry() {
    let root = workspace("patch-errors");
    fs::write(root.join("notes.txt"), b"alpha\n").expect("fixture should write");

    let missing_header = execute_patch(
        &root,
        &PatchRequest {
            patch: format!("SWAP 1:{}:\n+omega", line_hash("alpha")),
            dry_run: true,
        },
    )
    .expect_err("an existing-file patch without a section header should fail")
    .to_string();
    for required in [
        "Hashline patch program line 1",
        "[path]#HASH",
        "Hashline dialect",
    ] {
        assert!(
            missing_header.contains(required),
            "missing-header error should document {required}"
        );
    }

    let read_result = read(
        &root,
        &ReadRequest {
            path: "notes.txt".to_owned(),
            start_line: None,
            end_line: None,
            max_lines: None,
        },
    )
    .expect("fixture should read");
    let unified_diff = execute_patch(
        &root,
        &PatchRequest {
            patch: format!(
                "{}\n@@ -1 +1 @@\n-alpha\n+omega",
                read_result["header"]
                    .as_str()
                    .expect("header should be text")
            ),
            dry_run: true,
        },
    )
    .expect_err("unified diff syntax should fail")
    .to_string();
    for required in ["hashline__read", "[path]#HASH", "SWAP 12:abcd"] {
        assert!(
            unified_diff.contains(required),
            "unified-diff error should document {required}"
        );
    }

    let absolute_path = read(
        &root,
        &ReadRequest {
            path: root.join("notes.txt").display().to_string(),
            start_line: None,
            end_line: None,
            max_lines: None,
        },
    )
    .expect_err("absolute paths should fail")
    .to_string();
    assert!(absolute_path.contains("workspace-relative"));
    assert!(absolute_path.contains("invalid `path`"));

    let patch_path = execute_patch(
        &root,
        &PatchRequest {
            patch: "[/absolute.txt]\nINS.HEAD:\n+nope".to_owned(),
            dry_run: false,
        },
    )
    .expect_err("an invalid patch section path should fail")
    .to_string();
    assert!(patch_path.contains("invalid `path` at Hashline patch program line 1"));
    assert!(!root.join("absolute.txt").exists());

    let invalid_root = execute_transaction(
        &root,
        &TransactionRequest {
            action: TransactionAction::Preview,
            root: Some("/absolute".to_owned()),
            mutations: vec![FileMutation::Create {
                path: "created.txt".to_owned(),
                contents: "nope".to_owned(),
            }],
        },
    )
    .expect_err("an invalid transaction root should fail")
    .to_string();
    assert!(invalid_root.contains("invalid `root`"));

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
    let native = super::transaction_fs::open_root(&root, ".").expect("root should be supported");
    let lease = super::transaction_fs::lock_paths(&native, &prepared)
        .expect("transaction paths should lock");
    let mut journal = super::transaction_fs::write_journal(&native, "recovery-fixture", &prepared)
        .expect("journal should be durable");
    super::transaction_fs::apply_prepared(&native, &lease, &prepared, &mut journal)
        .expect("mutations should apply before simulated interruption");
    drop(journal);
    drop(lease);

    super::transaction_fs::recover_pending(&native).expect("fresh recovery should converge");

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

#[cfg(target_os = "linux")]
#[test]
fn transaction_plan_digest_binds_root_identity() {
    let workspace = workspace("root-identity");
    fs::create_dir(workspace.join("selected")).expect("selected root should be created");
    fs::write(workspace.join("selected/a.txt"), "before\n").expect("fixture should write");
    let request = TransactionRequest {
        action: TransactionAction::Preview,
        root: Some("selected".to_owned()),
        mutations: vec![FileMutation::Update {
            path: "a.txt".to_owned(),
            expected: super::ExpectedFile {
                exact_digest: exact_digest(b"before\n"),
            },
            edits: vec![FileEdit::ReplaceAll {
                contents: "after\n".to_owned(),
            }],
        }],
    };
    let first = execute_transaction(&workspace, &request).expect("first preview should succeed");
    fs::rename(workspace.join("selected"), workspace.join("replaced"))
        .expect("selected root should move");
    fs::create_dir(workspace.join("selected")).expect("replacement root should be created");
    fs::write(workspace.join("selected/a.txt"), "before\n").expect("replacement should match");
    let second = execute_transaction(&workspace, &request).expect("second preview should succeed");
    assert_ne!(first["planDigest"], second["planDigest"]);
    fs::remove_dir_all(workspace).expect("workspace should be removed");
}

#[cfg(target_os = "linux")]
#[test]
fn leased_parent_replacement_fails_without_mutating_either_directory() {
    let root = workspace("parent-replacement");
    fs::create_dir(root.join("dir")).expect("parent should be created");
    fs::write(root.join("dir/a.txt"), "before\n").expect("fixture should write");
    let native = super::transaction_fs::open_root(&root, ".").expect("root should be supported");
    let prepared = vec![super::PreparedMutation::Write {
        path: "dir/a.txt".to_owned(),
        before: Some(b"before\n".to_vec()),
        after: b"after\n".to_vec(),
    }];
    let lease =
        super::transaction_fs::lock_paths(&native, &prepared).expect("original parent should lock");
    fs::rename(root.join("dir"), root.join("original")).expect("parent should move");
    fs::create_dir(root.join("dir")).expect("replacement parent should be created");
    fs::write(root.join("dir/a.txt"), "before\n").expect("replacement fixture should write");
    let mut journal = super::transaction_fs::write_journal(&native, "parent-race", &prepared)
        .expect("journal should write");
    let error = super::transaction_fs::apply_prepared(&native, &lease, &prepared, &mut journal)
        .expect_err("replaced parent must fail");
    assert!(error.to_string().contains("not covered"));
    assert_eq!(
        fs::read(root.join("original/a.txt")).expect("original should read"),
        b"before\n"
    );
    assert_eq!(
        fs::read(root.join("dir/a.txt")).expect("replacement should read"),
        b"before\n"
    );
    drop(journal);
    drop(lease);
    fs::remove_dir_all(root).expect("workspace should be removed");
}

#[cfg(target_os = "linux")]
#[test]
fn recovery_preserves_unrecognized_external_state_and_retains_evidence() {
    let root = workspace("recovery-conflict");
    fs::write(root.join("a.txt"), "before\n").expect("fixture should write");
    let native = super::transaction_fs::open_root(&root, ".").expect("root should be supported");
    let prepared = vec![super::PreparedMutation::Write {
        path: "a.txt".to_owned(),
        before: Some(b"before\n".to_vec()),
        after: b"after\n".to_vec(),
    }];
    let lease = super::transaction_fs::lock_paths(&native, &prepared)
        .expect("transaction path should lock");
    let mut journal = super::transaction_fs::write_journal(&native, "conflict", &prepared)
        .expect("journal should write");
    super::transaction_fs::apply_prepared(&native, &lease, &prepared, &mut journal)
        .expect("mutation should apply");
    drop(journal);
    drop(lease);
    fs::write(root.join("a.txt"), "external\n").expect("external state should write");
    let error = super::transaction_fs::recover_pending(&native)
        .expect_err("unrecognized state must stop recovery");
    assert!(error.to_string().contains("manual recovery is required"));
    assert_eq!(
        fs::read(root.join("a.txt")).expect("state should read"),
        b"external\n"
    );
    assert!(
        root.join(".nanocodex/hashline-transactions/conflict.json")
            .exists()
    );
    fs::remove_dir_all(root).expect("workspace should be removed");
}

#[cfg(target_os = "linux")]
#[test]
fn cross_process_coordination_conflicts_only_for_shared_parents() {
    let root = workspace("coordination");
    fs::create_dir(root.join("left")).expect("left should be created");
    fs::create_dir(root.join("right")).expect("right should be created");
    fs::write(root.join("left/a.txt"), "left\n").expect("left fixture should write");
    fs::write(root.join("right/b.txt"), "right\n").expect("right fixture should write");
    let marker = root.join("lease-ready");
    let mut child = Command::new(std::env::current_exe().expect("test executable should resolve"))
        .args([
            "--ignored",
            "--exact",
            "hashline::tests::transaction_lease_child",
            "--nocapture",
        ])
        .env("NANOCODEX_HASHLINE_CHILD_ROOT", &root)
        .env("NANOCODEX_HASHLINE_CHILD_MARKER", &marker)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("lease child should start");
    for _ in 0..200 {
        if marker.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(marker.exists(), "lease child did not become ready");

    let native = super::transaction_fs::open_root(&root, ".").expect("root should be supported");
    let overlapping = vec![super::PreparedMutation::Write {
        path: "left/other.txt".to_owned(),
        before: None,
        after: b"other\n".to_vec(),
    }];
    let conflict = super::transaction_fs::lock_paths(&native, &overlapping)
        .expect_err("same parent must conflict");
    assert!(conflict.to_string().contains("transaction conflict"));

    let disjoint = vec![super::PreparedMutation::Write {
        path: "right/other.txt".to_owned(),
        before: None,
        after: b"other\n".to_vec(),
    }];
    let lease = super::transaction_fs::lock_paths(&native, &disjoint)
        .expect("disjoint parent should lock concurrently");
    drop(lease);
    child
        .stdin
        .as_mut()
        .expect("child stdin should be piped")
        .write_all(b"release\n")
        .expect("child should receive release");
    assert!(child.wait().expect("child should finish").success());
    fs::remove_dir_all(root).expect("workspace should be removed");
}

#[cfg(target_os = "linux")]
#[test]
fn subprocess_faults_at_every_durable_transition_converge_and_clean_up() {
    const FAULTS: &[&str] = &[
        "prepared-journal-file-sync",
        "prepared-journal-publish",
        "prepared-journal-dir-sync",
        "mutation-0-after-stage-sync",
        "mutation-0-after-rename",
        "mutation-0-after-parent-sync",
        "after-mutation-0",
        "progress-0-journal-file-sync",
        "progress-0-journal-publish",
        "progress-0-journal-dir-sync",
        "mutation-1-after-stage-sync",
        "mutation-1-after-rename",
        "mutation-1-after-parent-sync",
        "after-mutation-1",
        "progress-1-journal-file-sync",
        "progress-1-journal-publish",
        "progress-1-journal-dir-sync",
        "mutation-2-after-unlink",
        "mutation-2-after-parent-sync",
        "after-mutation-2",
        "progress-2-journal-file-sync",
        "progress-2-journal-publish",
        "progress-2-journal-dir-sync",
        "mutation-3-destination-after-stage-sync",
        "mutation-3-destination-after-rename",
        "mutation-3-destination-after-parent-sync",
        "mutation-3-after-source-unlink",
        "mutation-3-after-source-parent-sync",
        "after-mutation-3",
        "progress-3-journal-file-sync",
        "progress-3-journal-publish",
        "progress-3-journal-dir-sync",
        "before-journal-remove",
        "after-journal-remove",
        "after-journal-remove-dir-sync",
    ];
    for fault in FAULTS {
        let root = workspace(&format!("fault-{}", fault.replace('-', "_")));
        fs::write(root.join("a.txt"), "before-a\n").expect("a fixture should write");
        fs::write(root.join("delete.txt"), "before-delete\n").expect("delete fixture should write");
        fs::write(root.join("move.txt"), "before-move\n").expect("move fixture should write");
        let status = Command::new(std::env::current_exe().expect("test executable should resolve"))
            .args([
                "--ignored",
                "--exact",
                "hashline::tests::transaction_fault_child",
                "--nocapture",
            ])
            .env("NANOCODEX_HASHLINE_CHILD_ROOT", &root)
            .env("NANOCODEX_HASHLINE_FAULT", fault)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("fault child should run");
        assert!(
            !status.success(),
            "fault point {fault} did not interrupt the child"
        );
        let native = super::transaction_fs::open_root(&root, ".").expect("root should reopen");
        super::transaction_fs::recover_pending(&native).expect("recovery should converge");
        assert_recovered_state(&root, fault);
        assert!(
            !root.join(".nanocodex").exists(),
            "fault point {fault} left recovery storage"
        );
        let temporary = fs::read_dir(&root)
            .expect("workspace should list")
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains("nanocodex-hashline")
            });
        assert!(!temporary, "fault point {fault} left a temporary file");
        fs::remove_dir_all(root).expect("workspace should be removed");
    }
}

#[cfg(target_os = "linux")]
fn assert_recovered_state(root: &std::path::Path, fault: &str) {
    let state = (
        fs::read(root.join("a.txt")).expect("a should read"),
        fs::read(root.join("create.txt")).ok(),
        fs::read(root.join("delete.txt")).ok(),
        fs::read(root.join("move.txt")).ok(),
        fs::read(root.join("moved.txt")).ok(),
    );
    let all_before = (
        b"before-a\n".to_vec(),
        None,
        Some(b"before-delete\n".to_vec()),
        Some(b"before-move\n".to_vec()),
        None,
    );
    let all_after = (
        b"after-a\n".to_vec(),
        Some(b"created\n".to_vec()),
        None,
        None,
        Some(b"after-move\n".to_vec()),
    );
    assert!(
        state == all_before || state == all_after,
        "fault point {fault} left a mixed state: {state:?}"
    );
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "subprocess helper"]
fn transaction_fault_child() {
    let root = PathBuf::from(
        std::env::var_os("NANOCODEX_HASHLINE_CHILD_ROOT")
            .expect("child root environment should be set"),
    );
    let request: TransactionRequest = serde_json::from_value(json!({
        "action": {"type": "commit"},
        "mutations": [
            {
                "type": "update",
                "path": "a.txt",
                "expected": {"exactDigest": exact_digest(b"before-a\n")},
                "edits": [{"type": "replaceAll", "contents": "after-a\n"}]
            },
            {
                "type": "create",
                "path": "create.txt",
                "contents": "created\n"
            },
            {
                "type": "delete",
                "path": "delete.txt",
                "expected": {"exactDigest": exact_digest(b"before-delete\n")}
            },
            {
                "type": "move",
                "source": "move.txt",
                "destination": "moved.txt",
                "expected": {"exactDigest": exact_digest(b"before-move\n")},
                "edits": [{"type": "replaceAll", "contents": "after-move\n"}]
            }
        ]
    }))
    .expect("child request should decode");
    let _ = execute_transaction(&root, &request);
    panic!("configured fault point was not reached");
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "subprocess helper"]
fn transaction_lease_child() {
    let root = PathBuf::from(
        std::env::var_os("NANOCODEX_HASHLINE_CHILD_ROOT")
            .expect("child root environment should be set"),
    );
    let marker = PathBuf::from(
        std::env::var_os("NANOCODEX_HASHLINE_CHILD_MARKER")
            .expect("child marker environment should be set"),
    );
    let native = super::transaction_fs::open_root(&root, ".").expect("root should be supported");
    let prepared = vec![super::PreparedMutation::Write {
        path: "left/a.txt".to_owned(),
        before: Some(b"left\n".to_vec()),
        after: b"changed\n".to_vec(),
    }];
    let _lease =
        super::transaction_fs::lock_paths(&native, &prepared).expect("left parent should lock");
    fs::write(marker, "ready").expect("marker should write");
    let mut release = String::new();
    std::io::stdin()
        .read_line(&mut release)
        .expect("release should read");
}
