use std::fs;
use std::io::Write as _;
#[cfg(target_os = "linux")]
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
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
    assert!(result.get("header").is_none());
    assert_eq!(result["content"], "1:93c8|alpha\n2:f589|beta");
    let patch_header = result["patchHeader"]
        .as_str()
        .expect("patch header should be text");
    assert_eq!(
        patch_header,
        format!("[notes.txt]#{}", result["hash"].as_str().unwrap())
    );
    assert_eq!(result["hash"].as_str().map(str::len), Some(8));
    assert_eq!(result["exactDigest"].as_str().map(str::len), Some(64));
    let patch = format!("{patch_header}\nSWAP 2:{}:\n+bravo", line_hash("beta"));
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
fn split_patch_input_previews_applies_and_rejects_ambiguous_shapes() {
    let root = workspace("split-patch");
    fs::write(root.join("notes.txt"), b"alpha\nbeta\n").expect("fixture should write");
    let observed = read(
        &root,
        &ReadRequest {
            path: "notes.txt".to_owned(),
            start_line: None,
            end_line: None,
            max_lines: None,
        },
    )
    .expect("read should succeed");
    let header = observed["patchHeader"]
        .as_str()
        .expect("patch header should be text");
    let operations = format!("SWAP 2:{}:\n+bravo", line_hash("beta"));
    let dry_run = super::decode_patch_request(
        &json!({"header": header, "operations": operations, "dry_run": true}).to_string(),
    )
    .expect("split dry-run input should decode");
    execute_patch(&root, &dry_run).expect("split dry-run should preview");
    assert_eq!(
        fs::read(root.join("notes.txt")).expect("fixture should read"),
        b"alpha\nbeta\n"
    );
    let apply = super::decode_patch_request(
        &json!({"header": header, "operations": operations}).to_string(),
    )
    .expect("split apply input should decode");
    execute_patch(&root, &apply).expect("split apply should succeed");
    assert_eq!(
        fs::read(root.join("notes.txt")).expect("result should read"),
        b"alpha\nbravo\n"
    );

    for rejected in [
        json!({}),
        json!({"header": header}),
        json!({"operations": operations}),
        json!({"patch": "ABORT", "header": header, "operations": operations}),
        json!({"header": format!("{header}\n[other.txt]"), "operations": "INS.HEAD:\n+x"}),
        json!({"header": header, "operations": "[other.txt]\nINS.HEAD:\n+x"}),
    ] {
        assert!(
            super::decode_patch_request(&rejected.to_string()).is_err(),
            "ambiguous or multi-file split input should fail"
        );
    }
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
        read["description"]
            .as_str()
            .expect("read description should be text")
            .contains("patchHeader")
    );
    assert!(
        read["parameters"]["properties"]["path"]["description"]
            .as_str()
            .expect("read path description should be text")
            .contains("workspace-relative")
    );

    let patch =
        serde_json::to_value(super::patch_definition()).expect("patch definition should serialize");
    let properties = patch["parameters"]["properties"]
        .as_object()
        .expect("patch properties should be an object");
    assert_eq!(properties.len(), 4);
    for property in ["patch", "header", "operations", "dry_run"] {
        assert!(properties.contains_key(property));
    }
    assert_eq!(
        patch["parameters"]["oneOf"],
        json!([
            {
                "required": ["patch"],
                "not": {"anyOf": [
                    {"required": ["header"]},
                    {"required": ["operations"]}
                ]}
            },
            {
                "required": ["header", "operations"],
                "not": {"required": ["patch"]}
            }
        ])
    );
    let patch_contract = patch.to_string();
    for required in [
        "patchHeader",
        "[path]#HASH",
        "SWAP",
        "REM",
        "MV",
        "fully sectioned",
    ] {
        assert!(
            patch_contract.contains(required),
            "patch schema should document {required}"
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
        initial["patchHeader"]
            .as_str()
            .expect("patch header should be text"),
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
                fresh["patchHeader"]
                    .as_str()
                    .expect("patch header should be text"),
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
                read_result["patchHeader"]
                    .as_str()
                    .expect("patch header should be text")
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

    let changed_mutation: TransactionRequest = serde_json::from_value(json!({
        "action": {"type": "commitPreviewed", "expectedPlanDigest": digest.clone()},
        "mutations": [{
            "type": "update",
            "path": "a.txt",
            "expected": {"exactDigest": exact_digest(before)},
            "edits": [{"type": "replaceAll", "contents": "changed mutation\n"}]
        }]
    }))
    .expect("changed request should decode");
    let changed_error = execute_transaction(&root, &changed_mutation)
        .expect_err("changed mutations must not commit under the preview digest");
    assert!(changed_error.to_string().contains("plan digest mismatch"));
    assert_eq!(
        fs::read(root.join("a.txt")).expect("file should read"),
        before
    );

    fs::write(root.join("a.txt"), "externally changed\n").expect("external write should succeed");
    let stale_request: TransactionRequest = serde_json::from_value(json!({
        "action": {"type": "commitPreviewed", "expectedPlanDigest": digest.clone()},
        "mutations": [{
            "type": "update",
            "path": "a.txt",
            "expected": {"exactDigest": exact_digest(before)},
            "edits": [{"type": "insertAfter", "anchor": {"line": 1, "expectedHash": line_hash("alpha")}, "lines": ["inserted"]}]
        }]
    }))
    .expect("stale request should decode");
    execute_transaction(&root, &stale_request)
        .expect_err("changed files must invalidate a previewed commit");
    assert_eq!(
        fs::read(root.join("a.txt")).expect("external state should read"),
        b"externally changed\n"
    );
    fs::write(root.join("a.txt"), before).expect("fixture should restore");

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
    assert_terminal_receipt(&root, "committed");
    fs::remove_dir_all(root).expect("workspace should be removed");
}

#[test]
fn committed_journal_completes_commit_in_a_fresh_invocation() {
    let root = workspace("recovery");
    fs::write(root.join("a.txt"), "before-a\n").expect("fixture should write");
    fs::write(root.join("b.txt"), "before-b\n").expect("fixture should write");
    let prepared = vec![
        super::PreparedMutation::Write {
            path: "a.txt".to_owned(),
            before: Some(b"before-a\n".to_vec()),
            after: b"after-a\n".to_vec(),
            metadata: None,
        },
        super::PreparedMutation::Write {
            path: "b.txt".to_owned(),
            before: Some(b"before-b\n".to_vec()),
            after: b"after-b\n".to_vec(),
            metadata: None,
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
        b"after-a\n"
    );
    assert_eq!(
        fs::read(root.join("b.txt")).expect("file should read"),
        b"after-b\n"
    );
    assert_terminal_receipt(&root, "committed");
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
        metadata: None,
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
fn prepared_recovery_leaves_later_external_state_untouched() {
    let root = workspace("recovery-conflict");
    fs::write(root.join("a.txt"), "before\n").expect("fixture should write");
    let native = super::transaction_fs::open_root(&root, ".").expect("root should be supported");
    let prepared = vec![super::PreparedMutation::Write {
        path: "a.txt".to_owned(),
        before: Some(b"before\n".to_vec()),
        after: b"after\n".to_vec(),
        metadata: None,
    }];
    let lease = super::transaction_fs::lock_paths(&native, &prepared)
        .expect("transaction path should lock");
    let journal = super::transaction_fs::write_journal(&native, "conflict", &prepared)
        .expect("journal should write");
    drop(journal);
    drop(lease);
    fs::write(root.join("a.txt"), "external\n").expect("external state should write");
    super::transaction_fs::recover_pending(&native)
        .expect("prepared transaction has no visible state to roll back");
    assert_eq!(
        fs::read(root.join("a.txt")).expect("state should read"),
        b"external\n"
    );
    assert_terminal_receipt(&root, "rolledBack");
    fs::remove_dir_all(root).expect("workspace should be removed");
}

#[cfg(target_os = "linux")]
#[test]
fn artifacts_are_owner_only_and_live_failure_restores_prior_bytes_and_mode() {
    let root = workspace("metadata-rollback");
    fs::write(root.join("a.txt"), "before-a\n").expect("a fixture should write");
    fs::write(root.join("b.txt"), "before-b\n").expect("b fixture should write");
    fs::set_permissions(root.join("a.txt"), fs::Permissions::from_mode(0o751))
        .expect("a mode should set");
    fs::set_permissions(root.join("b.txt"), fs::Permissions::from_mode(0o640))
        .expect("b mode should set");
    let prepared = vec![
        super::PreparedMutation::Write {
            path: "a.txt".to_owned(),
            before: Some(b"before-a\n".to_vec()),
            after: b"after-a\n".to_vec(),
            metadata: Some(super::FileMetadata { mode: Some(0o751) }),
        },
        super::PreparedMutation::Write {
            path: "b.txt".to_owned(),
            before: Some(b"before-b\n".to_vec()),
            after: b"after-b\n".to_vec(),
            metadata: Some(super::FileMetadata { mode: Some(0o640) }),
        },
    ];
    let native = super::transaction_fs::open_root(&root, ".").expect("root should open");
    let lease = super::transaction_fs::lock_paths(&native, &prepared).expect("paths should lock");
    let mut journal = super::transaction_fs::write_journal(&native, "metadata-rollback", &prepared)
        .expect("journal should write");
    let storage = root.join(".nanocodex/hashline-transactions");
    for entry in fs::read_dir(&storage).expect("storage should list") {
        let entry = entry.expect("entry should list");
        assert_eq!(
            entry.metadata().expect("entry metadata should read").mode() & 0o777,
            0o600
        );
    }
    let manifest =
        fs::read_to_string(storage.join("metadata-rollback.json")).expect("manifest should read");
    assert!(!manifest.contains("before-a"));
    assert!(!manifest.contains("after-a"));
    let duplicate = super::transaction_fs::write_journal(&native, "metadata-rollback", &prepared)
        .expect_err("duplicate transaction ID must fail");
    assert!(duplicate.to_string().contains("duplicate transaction ID"));

    fs::write(root.join("b.txt"), "external-b\n").expect("external bytes should write");
    fs::set_permissions(root.join("b.txt"), fs::Permissions::from_mode(0o600))
        .expect("external mode should set");
    let error = super::transaction_fs::apply_prepared(&native, &lease, &prepared, &mut journal)
        .expect_err("second mutation should reject disturbed evidence");
    assert!(error.to_string().contains("rollback requires recovery"));
    assert_eq!(
        fs::read(root.join("a.txt")).expect("a should read"),
        b"before-a\n"
    );
    assert_eq!(
        fs::metadata(root.join("a.txt"))
            .expect("a metadata should read")
            .mode()
            & 0o7777,
        0o751
    );
    assert_eq!(
        fs::read(root.join("b.txt")).expect("b should read"),
        b"external-b\n"
    );
    assert_eq!(
        fs::metadata(root.join("b.txt"))
            .expect("b metadata should read")
            .mode()
            & 0o7777,
        0o600
    );
    drop(journal);
    drop(lease);
    let retained: serde_json::Value = serde_json::from_slice(
        &fs::read(storage.join("metadata-rollback.json")).expect("journal should remain"),
    )
    .expect("journal should decode");
    assert_eq!(retained["state"], "recoveryRequired");
    assert!(fs::read_dir(&storage).expect("storage should list").count() > 2);
    fs::remove_dir_all(root).expect("workspace should be removed");
}

#[cfg(target_os = "linux")]
#[test]
fn commit_preserves_update_and_move_permissions() {
    let root = workspace("metadata-commit");
    fs::write(root.join("update.txt"), "before-update\n").expect("update fixture should write");
    fs::write(root.join("move.txt"), "before-move\n").expect("move fixture should write");
    fs::set_permissions(root.join("update.txt"), fs::Permissions::from_mode(0o751))
        .expect("update mode should set");
    fs::set_permissions(root.join("move.txt"), fs::Permissions::from_mode(0o711))
        .expect("move mode should set");
    let request: TransactionRequest = serde_json::from_value(json!({
        "action": {"type": "commit"},
        "mutations": [
            {
                "type": "update",
                "path": "update.txt",
                "expected": {"exactDigest": exact_digest(b"before-update\n")},
                "edits": [{"type": "replaceAll", "contents": "after-update\n"}]
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
    .expect("request should decode");
    execute_transaction(&root, &request).expect("transaction should commit");
    assert_eq!(
        fs::metadata(root.join("update.txt"))
            .expect("update metadata should read")
            .mode()
            & 0o7777,
        0o751
    );
    assert_eq!(
        fs::metadata(root.join("moved.txt"))
            .expect("move metadata should read")
            .mode()
            & 0o7777,
        0o711
    );
    fs::remove_dir_all(root).expect("workspace should be removed");
}

#[cfg(target_os = "linux")]
#[test]
fn terminal_receipts_are_bounded_and_oldest_ids_are_pruned() {
    let root = workspace("receipt-bound");
    let native = super::transaction_fs::open_root(&root, ".").expect("root should open");
    for index in 0..65 {
        let directory = format!("directory-{index:03}");
        fs::create_dir(root.join(&directory)).expect("transaction directory should create");
        let path = format!("{directory}/created.txt");
        let prepared = vec![super::PreparedMutation::Write {
            path: path.clone(),
            before: None,
            after: format!("{index}\n").into_bytes(),
            metadata: None,
        }];
        let lease =
            super::transaction_fs::lock_paths(&native, &prepared).expect("path should lock");
        let transaction_id = format!("receipt-{index:03}");
        let mut journal = super::transaction_fs::write_journal(&native, &transaction_id, &prepared)
            .expect("journal should write");
        super::transaction_fs::apply_prepared(&native, &lease, &prepared, &mut journal)
            .expect("mutation should apply");
        super::transaction_fs::remove_journal(&native, journal)
            .expect("transaction should terminalize");
        drop(lease);
    }
    let storage = root.join(".nanocodex/hashline-transactions");
    let entries = fs::read_dir(&storage)
        .expect("storage should list")
        .collect::<Result<Vec<_>, _>>()
        .expect("entries should list");
    assert_eq!(entries.len(), 128);
    assert!(!storage.join("receipt-000.json").exists());
    assert!(!storage.join("receipt-000.reserve").exists());
    assert!(storage.join("receipt-064.json").exists());
    assert!(storage.join("receipt-064.reserve").exists());
    fs::remove_dir_all(root).expect("workspace should be removed");
}

#[cfg(target_os = "linux")]
#[test]
fn malformed_recovery_entry_does_not_prevent_clean_entry_recovery() {
    let root = workspace("isolated-recovery");
    let native = super::transaction_fs::open_root(&root, ".").expect("root should open");
    let prepared = vec![super::PreparedMutation::Write {
        path: "created.txt".to_owned(),
        before: None,
        after: b"created\n".to_vec(),
        metadata: None,
    }];
    let bad = super::transaction_fs::write_journal(&native, "a-bad", &prepared)
        .expect("bad fixture should prepare");
    let clean = super::transaction_fs::write_journal(&native, "b-clean", &prepared)
        .expect("clean fixture should prepare");
    drop(bad);
    drop(clean);
    let storage = root.join(".nanocodex/hashline-transactions");
    fs::write(storage.join("a-bad.json"), b"{not-json").expect("bad fixture should be disturbed");
    super::transaction_fs::recover_pending(&native)
        .expect_err("malformed entry should be reported");
    let clean: serde_json::Value = serde_json::from_slice(
        &fs::read(storage.join("b-clean.json")).expect("clean receipt should read"),
    )
    .expect("clean receipt should decode");
    assert_eq!(clean["state"], "complete");
    assert_eq!(clean["outcome"], "rolledBack");
    assert_eq!(
        fs::read(storage.join("a-bad.json")).expect("bad evidence should remain"),
        b"{not-json"
    );
    fs::remove_dir_all(root).expect("workspace should be removed");
}

#[cfg(target_os = "linux")]
#[test]
fn disturbed_artifact_is_retained_and_never_applied() {
    let root = workspace("artifact-disturbance");
    let native = super::transaction_fs::open_root(&root, ".").expect("root should open");
    let prepared = vec![super::PreparedMutation::Write {
        path: "created.txt".to_owned(),
        before: None,
        after: b"secret-body\n".to_vec(),
        metadata: None,
    }];
    let journal = super::transaction_fs::write_journal(&native, "artifact", &prepared)
        .expect("fixture should prepare");
    drop(journal);
    let storage = root.join(".nanocodex/hashline-transactions");
    let artifact = fs::read_dir(&storage)
        .expect("storage should list")
        .map(|entry| entry.expect("entry should list").path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "staged")
        })
        .expect("staged artifact should exist");
    fs::write(&artifact, b"disturbed\n").expect("artifact should be disturbed");
    let error = super::transaction_fs::recover_pending(&native)
        .expect_err("disturbed artifact must stop this transaction");
    assert!(error.to_string().contains("artifact"));
    assert!(!root.join("created.txt").exists());
    assert_eq!(
        fs::read(&artifact).expect("disturbed artifact should remain"),
        b"disturbed\n"
    );
    fs::remove_dir_all(root).expect("workspace should be removed");
}

#[cfg(target_os = "linux")]
#[test]
fn transaction_preview_output_is_capped_before_structural_evidence() {
    let root = workspace("preview-bound");
    let mut mutations = Vec::new();
    for index in 0..10 {
        let path = format!("file-{index}.txt");
        let before = format!("{}\n", "a".repeat(4_000));
        fs::write(root.join(&path), &before).expect("fixture should write");
        mutations.push(FileMutation::Update {
            path,
            expected: super::ExpectedFile {
                exact_digest: exact_digest(before.as_bytes()),
            },
            edits: vec![FileEdit::ReplaceAll {
                contents: format!("{}\n", "b".repeat(4_000)),
            }],
        });
    }
    let preview = execute_transaction(
        &root,
        &TransactionRequest {
            action: TransactionAction::Preview,
            root: None,
            mutations,
        },
    )
    .expect("preview should succeed");
    assert_eq!(preview["preview_truncated"], true);
    assert!(
        serde_json::to_vec(&preview)
            .expect("preview should encode")
            .len()
            <= 8 * 1024
    );
    assert!(
        preview["mutations"]
            .as_array()
            .expect("mutations should be an array")
            .len()
            < 10
    );
    fs::remove_dir_all(root).expect("workspace should be removed");
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "current_thread")]
async fn embedded_handler_keeps_executor_responsive_during_blocking_filesystem_work() {
    use super::Tool as _;

    let root = workspace("spawn-blocking");
    fs::write(root.join("large.txt"), vec![b'a'; 4 * 1024 * 1024])
        .expect("large fixture should write");
    let handler = super::HashlineHandler::new(root.clone(), super::HashlineToolKind::Read);
    let input = super::ToolInput::Function(
        serde_json::value::RawValue::from_string(r#"{"path":"large.txt"}"#.to_owned())
            .expect("input should be raw JSON"),
    );
    let context = super::ToolContext {
        model: "test-model",
        session_id: "test-session",
        call_id: "test-call",
        history: &[],
        output_token_budget: crate::DEFAULT_TOOL_OUTPUT_TOKENS,
    };
    let execution = handler.execute(input, context);
    tokio::pin!(execution);
    tokio::select! {
        _ = &mut execution => panic!("large filesystem call did not yield the current-thread executor"),
        () = tokio::time::sleep(Duration::from_millis(1)) => {}
    }
    execution.await.expect("handler should complete");
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
        metadata: None,
    }];
    let conflict = super::transaction_fs::lock_paths(&native, &overlapping)
        .expect_err("same parent must conflict");
    assert!(conflict.to_string().contains("transaction conflict"));

    let disjoint = vec![super::PreparedMutation::Write {
        path: "right/other.txt".to_owned(),
        before: None,
        after: b"other\n".to_vec(),
        metadata: None,
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
fn assert_terminal_receipt(root: &std::path::Path, expected_outcome: &str) {
    let directory = root.join(".nanocodex/hashline-transactions");
    let mut entries = fs::read_dir(&directory)
        .expect("transaction storage should exist")
        .map(|entry| {
            entry
                .expect("transaction entry should list")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    entries.sort();
    assert_eq!(
        entries.len(),
        2,
        "only a terminal receipt and reservation remain"
    );
    assert!(entries.iter().any(|name| {
        std::path::Path::new(name)
            .extension()
            .is_some_and(|extension| extension == "reserve")
    }));
    let journal_name = entries
        .iter()
        .find(|name| {
            std::path::Path::new(name)
                .extension()
                .is_some_and(|extension| extension == "json")
        })
        .expect("terminal journal should remain");
    let journal: serde_json::Value = serde_json::from_slice(
        &fs::read(directory.join(journal_name)).expect("terminal journal should read"),
    )
    .expect("terminal journal should decode");
    assert_eq!(journal["state"], "complete");
    assert_eq!(journal["outcome"], expected_outcome);
    let encoded = serde_json::to_string(&journal).expect("journal should encode");
    assert!(!encoded.contains("before-a"));
    assert!(!encoded.contains("after-a"));
}

#[cfg(target_os = "linux")]
fn assert_no_live_transaction_artifacts(root: &std::path::Path, fault: &str) {
    let directory = root.join(".nanocodex/hashline-transactions");
    if !directory.exists() {
        return;
    }
    for entry in fs::read_dir(&directory).expect("transaction storage should list") {
        let name = entry
            .expect("transaction entry should list")
            .file_name()
            .to_string_lossy()
            .into_owned();
        assert!(
            std::path::Path::new(&name)
                .extension()
                .is_some_and(|extension| extension == "json" || extension == "reserve"),
            "fault point {fault} left live artifact {name}"
        );
        if std::path::Path::new(&name)
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let journal: serde_json::Value = serde_json::from_slice(
                &fs::read(root.join(".nanocodex/hashline-transactions").join(&name))
                    .expect("journal should read"),
            )
            .expect("journal should decode");
            assert_eq!(journal["state"], "complete");
        }
    }
}

#[cfg(target_os = "linux")]
fn set_fault_fixture_modes(root: &std::path::Path) {
    fs::set_permissions(root.join("a.txt"), fs::Permissions::from_mode(0o751))
        .expect("a mode should set");
    fs::set_permissions(root.join("delete.txt"), fs::Permissions::from_mode(0o711))
        .expect("delete mode should set");
    fs::set_permissions(root.join("move.txt"), fs::Permissions::from_mode(0o700))
        .expect("move mode should set");
}

#[cfg(target_os = "linux")]
fn assert_fault_fixture_modes(root: &std::path::Path) {
    assert_eq!(
        fs::metadata(root.join("a.txt"))
            .expect("a metadata should read")
            .mode()
            & 0o7777,
        0o751
    );
    if root.join("delete.txt").exists() {
        assert_eq!(
            fs::metadata(root.join("delete.txt"))
                .expect("delete metadata should read")
                .mode()
                & 0o7777,
            0o711
        );
    }
    let move_path = if root.join("move.txt").exists() {
        root.join("move.txt")
    } else {
        root.join("moved.txt")
    };
    assert_eq!(
        fs::metadata(move_path)
            .expect("move metadata should read")
            .mode()
            & 0o7777,
        0o700
    );
}

#[cfg(target_os = "linux")]
#[test]
fn subprocess_faults_at_every_durable_transition_converge_and_clean_up() {
    const FAULTS: &[&str] = &[
        "preparing-journal-file-sync",
        "preparing-journal-publish",
        "preparing-journal-dir-sync",
        "after-artifacts-dir-sync",
        "prepared-journal-file-sync",
        "prepared-journal-publish",
        "prepared-journal-dir-sync",
        "committing-journal-file-sync",
        "committing-journal-publish",
        "committing-journal-dir-sync",
        "mutation-0-after-stage-sync",
        "mutation-0-after-rename",
        "mutation-0-after-parent-sync",
        "after-mutation-0",
        "commit-progress-0-journal-file-sync",
        "commit-progress-0-journal-publish",
        "commit-progress-0-journal-dir-sync",
        "mutation-1-after-stage-sync",
        "mutation-1-after-rename",
        "mutation-1-after-parent-sync",
        "after-mutation-1",
        "commit-progress-1-journal-file-sync",
        "commit-progress-1-journal-publish",
        "commit-progress-1-journal-dir-sync",
        "mutation-2-after-unlink",
        "mutation-2-after-parent-sync",
        "after-mutation-2",
        "commit-progress-2-journal-file-sync",
        "commit-progress-2-journal-publish",
        "commit-progress-2-journal-dir-sync",
        "mutation-3-destination-after-stage-sync",
        "mutation-3-destination-after-rename",
        "mutation-3-destination-after-parent-sync",
        "mutation-3-after-source-unlink",
        "mutation-3-after-source-parent-sync",
        "after-mutation-3",
        "commit-progress-3-journal-file-sync",
        "commit-progress-3-journal-publish",
        "commit-progress-3-journal-dir-sync",
        "committed-journal-file-sync",
        "committed-journal-publish",
        "committed-journal-dir-sync",
        "cleaning-committed-journal-file-sync",
        "cleaning-committed-journal-publish",
        "cleaning-committed-journal-dir-sync",
        "complete-committed-journal-file-sync",
        "complete-committed-journal-publish",
        "complete-committed-journal-dir-sync",
    ];
    for fault in FAULTS {
        let root = workspace(&format!("fault-{}", fault.replace('-', "_")));
        fs::write(root.join("a.txt"), "before-a\n").expect("a fixture should write");
        fs::write(root.join("delete.txt"), "before-delete\n").expect("delete fixture should write");
        fs::write(root.join("move.txt"), "before-move\n").expect("move fixture should write");
        set_fault_fixture_modes(&root);
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
        assert_no_live_transaction_artifacts(&root, fault);
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
#[test]
fn subprocess_faults_during_reverse_rollback_converge_and_clean_up() {
    const FAULTS: &[&str] = &[
        "rolling-back-journal-file-sync",
        "rolling-back-journal-publish",
        "rolling-back-journal-dir-sync",
        "recover-move-after-stage-sync",
        "recover-move-after-rename",
        "recover-move-after-parent-sync",
        "recover-move-after-destination-unlink",
        "recover-move-after-destination-parent-sync",
        "rollback-progress-0-journal-file-sync",
        "rollback-progress-0-journal-publish",
        "rollback-progress-0-journal-dir-sync",
        "recover-delete-after-stage-sync",
        "recover-delete-after-rename",
        "recover-delete-after-parent-sync",
        "rollback-progress-1-journal-file-sync",
        "rollback-progress-1-journal-publish",
        "rollback-progress-1-journal-dir-sync",
        "recover-create-after-unlink",
        "recover-create-after-parent-sync",
        "rollback-progress-2-journal-file-sync",
        "rollback-progress-2-journal-publish",
        "rollback-progress-2-journal-dir-sync",
        "recover-write-after-stage-sync",
        "recover-write-after-rename",
        "recover-write-after-parent-sync",
        "rollback-progress-3-journal-file-sync",
        "rollback-progress-3-journal-publish",
        "rollback-progress-3-journal-dir-sync",
        "rolled-back-journal-file-sync",
        "rolled-back-journal-publish",
        "rolled-back-journal-dir-sync",
        "cleaning-rolled-back-journal-file-sync",
        "cleaning-rolled-back-journal-publish",
        "cleaning-rolled-back-journal-dir-sync",
        "complete-rolled-back-journal-file-sync",
        "complete-rolled-back-journal-publish",
        "complete-rolled-back-journal-dir-sync",
    ];
    for fault in FAULTS {
        let root = workspace(&format!("rollback-fault-{}", fault.replace('-', "_")));
        fs::write(root.join("a.txt"), "before-a\n").expect("a fixture should write");
        fs::write(root.join("delete.txt"), "before-delete\n").expect("delete fixture should write");
        fs::write(root.join("move.txt"), "before-move\n").expect("move fixture should write");
        set_fault_fixture_modes(&root);
        let initial =
            Command::new(std::env::current_exe().expect("test executable should resolve"))
                .args([
                    "--ignored",
                    "--exact",
                    "hashline::tests::transaction_fault_child",
                    "--nocapture",
                ])
                .env("NANOCODEX_HASHLINE_CHILD_ROOT", &root)
                .env("NANOCODEX_HASHLINE_FAULT", "after-mutation-3")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .expect("forward fault child should run");
        assert!(!initial.success());
        let recovery =
            Command::new(std::env::current_exe().expect("test executable should resolve"))
                .args([
                    "--ignored",
                    "--exact",
                    "hashline::tests::transaction_recovery_fault_child",
                    "--nocapture",
                ])
                .env("NANOCODEX_HASHLINE_CHILD_ROOT", &root)
                .env("NANOCODEX_HASHLINE_FAULT", fault)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .expect("recovery fault child should run");
        assert!(
            !recovery.success(),
            "rollback fault point {fault} did not interrupt recovery"
        );
        let native = super::transaction_fs::open_root(&root, ".").expect("root should reopen");
        super::transaction_fs::recover_pending(&native).expect("recovery should converge");
        assert_eq!(
            fs::read(root.join("a.txt")).expect("a should read"),
            b"before-a\n"
        );
        assert!(!root.join("create.txt").exists());
        assert_eq!(
            fs::read(root.join("delete.txt")).expect("delete should read"),
            b"before-delete\n"
        );
        assert_eq!(
            fs::read(root.join("move.txt")).expect("move should read"),
            b"before-move\n"
        );
        assert!(!root.join("moved.txt").exists());
        assert_fault_fixture_modes(&root);
        assert_no_live_transaction_artifacts(&root, fault);
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
    assert_fault_fixture_modes(root);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "subprocess helper"]
fn transaction_recovery_fault_child() {
    let root = PathBuf::from(
        std::env::var_os("NANOCODEX_HASHLINE_CHILD_ROOT")
            .expect("child root environment should be set"),
    );
    let native = super::transaction_fs::open_root(&root, ".").expect("root should open");
    let _ = super::transaction_fs::recover_pending(&native);
    panic!("configured recovery fault point was not reached");
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
        metadata: None,
    }];
    let _lease =
        super::transaction_fs::lock_paths(&native, &prepared).expect("left parent should lock");
    fs::write(marker, "ready").expect("marker should write");
    let mut release = String::new();
    std::io::stdin()
        .read_line(&mut release)
        .expect("release should read");
}
