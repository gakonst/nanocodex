use std::{
    io::{BufRead, BufReader, Read},
    time::Duration,
};

use nanocodex_oai_api::responses::{ContentItem, MessageRole};
use serde_json::Value;
use tempfile::tempdir;

use super::*;
use super::{
    load::{visible_rollout_event, visible_tool_call},
    store::{
        RolloutCommit,
        writer::{RolloutWriter, validate_context_window_transition, write_line},
    },
};

fn message(text: &str) -> ResponseItem {
    ResponseItem::message(
        MessageRole::User,
        [ContentItem::InputText { text: text.into() }],
    )
}

fn completed_turn(prompt: &str, final_message: &str) -> RolloutTurn {
    completed_turn_with_effort(prompt, final_message, Thinking::High)
}

fn completed_turn_with_effort(prompt: &str, final_message: &str, effort: Thinking) -> RolloutTurn {
    RolloutTurn::started(&Prompt::new(prompt), effort).completed(final_message.to_owned())
}

#[test]
fn child_rollout_policy_does_not_inherit_a_resume_path() {
    let resumed =
        RolloutConfig::new("/codex").resumed(PathBuf::from("/codex/sessions/parent.jsonl"));
    let child = resumed.for_new_thread();

    assert_eq!(child.codex_home(), Path::new("/codex"));
    assert!(child.resume_path.is_none());
}

#[test]
fn discovers_active_and_archived_rollouts_newest_first() {
    let home = tempdir().expect("temporary Codex home");
    let active_id = "019c0d31-c308-7d91-bff4-5dca82d15ac6";
    let archived_id = "019c0d31-c308-7d91-bff4-5dca82d15ac5";
    let ignored_id = "019c0d31-c308-7d91-bff4-5dca82d15ac4";
    let active = write_discoverable_rollout(home.path(), "sessions/2026/08/04", active_id);
    let archived =
        write_discoverable_rollout(home.path(), "archived_sessions/2026/08/03", archived_id);
    let ignored_directory = home.path().join("sessions/2026/08/02");
    std::fs::create_dir_all(&ignored_directory).expect("create ignored rollout directory");
    std::fs::write(
        ignored_directory.join(format!("rollout-2026-08-02T12-00-00-{ignored_id}.jsonl")),
        "not json\n",
    )
    .expect("write malformed rollout");
    std::fs::write(
        ignored_directory.join(format!(
            "rollout-2026-08-02T12-00-00-{ignored_id}.jsonl.zst"
        )),
        "compressed",
    )
    .expect("write unsupported compressed rollout");

    set_modified(&archived, 10);
    set_modified(&active, 20);
    let sessions = RolloutConfig::new(home.path())
        .list_sessions()
        .expect("discover rollouts");

    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].thread_id(), active_id);
    assert_eq!(sessions[0].workspace(), home.path().to_str());
    assert_eq!(sessions[0].preview(), Some("resume me"));
    assert!(!sessions[0].is_archived());
    assert_eq!(sessions[1].thread_id(), archived_id);
    assert!(sessions[1].is_archived());
}

#[test]
fn discovery_prefers_an_active_copy_of_a_duplicate_thread() {
    let home = tempdir().expect("temporary Codex home");
    let thread_id = "019c0d31-c308-7d91-bff4-5dca82d15ac6";
    let active = write_discoverable_rollout(home.path(), "sessions/2026/08/04", thread_id);
    let archived =
        write_discoverable_rollout(home.path(), "archived_sessions/2026/08/05", thread_id);
    set_modified(&active, 10);
    set_modified(&archived, 20);

    let sessions = RolloutConfig::new(home.path())
        .list_sessions()
        .expect("discover deduplicated rollouts");

    assert_eq!(sessions.len(), 1);
    assert!(!sessions[0].is_archived());
}

fn write_discoverable_rollout(home: &Path, relative: &str, thread_id: &str) -> PathBuf {
    let directory = home.join(relative);
    std::fs::create_dir_all(&directory).expect("create rollout directory");
    let path = directory.join(format!("rollout-2026-08-04T12-00-00-{thread_id}.jsonl"));
    let mut file = File::create(&path).expect("create discoverable rollout");
    write_line(
        &mut file,
        &serde_json::json!({
            "timestamp": "2026-08-04T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": thread_id,
                "cwd": home,
                "history_mode": "legacy",
                "context_window": {"window_id": "window-1"}
            }
        }),
    )
    .expect("write session metadata");
    write_line(
        &mut file,
        &serde_json::json!({
            "timestamp": "2026-08-04T12:00:01Z",
            "type": "event_msg",
            "payload": {
                "type": "user_message",
                "message": "resume\n  me"
            }
        }),
    )
    .expect("write user preview");
    write_line(
        &mut file,
        &serde_json::json!({
            "timestamp": "2026-08-04T12:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "resume me"}]
            }
        }),
    )
    .expect("write committed history");
    file.flush().expect("flush discoverable rollout");
    path
}

#[test]
fn prompt_previews_are_single_line_and_bounded() {
    assert_eq!(
        load::prompt_preview("  first\n\tsecond  "),
        Some("first second".to_owned())
    );
    let long = "x".repeat(200);
    assert_eq!(
        load::prompt_preview(&long)
            .expect("non-empty preview")
            .chars()
            .count(),
        160
    );
    assert_eq!(load::prompt_preview(" \n\t "), None);
    assert_eq!(
        load::prompt_preview("safe\u{1b}]52;c;payload\u{7} text"),
        Some("safe]52;c;payload text".to_owned())
    );
}

#[test]
fn compaction_windows_must_form_one_contiguous_lineage() {
    let valid = serde_json::json!({
        "window_number": 1,
        "first_window_id": "window-1",
        "previous_window_id": "window-1",
        "window_id": "window-2",
    });
    assert_eq!(
        validate_context_window_transition(&valid, "window-1", "window-1", 0)
            .expect("valid compaction lineage"),
        (1, "window-2".to_owned())
    );

    let mut invalid = valid;
    invalid["previous_window_id"] = Value::String("unrelated".to_owned());
    assert!(validate_context_window_transition(&invalid, "window-1", "window-1", 0).is_err());
}

fn set_modified(path: &Path, seconds: u64) {
    File::options()
        .write(true)
        .open(path)
        .expect("open rollout to set modification time")
        .set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)),
        )
        .expect("set rollout modification time");
}

fn recorder(home: &Path) -> RolloutRecorder {
    RolloutRecorder::create(
        &Handle::current(),
        RolloutCreate {
            config: &RolloutConfig::new(home),
            thread_id: "019c0d31-c308-7d91-bff4-5dca82d15ac6",
            prompt_cache_key: "durable-cache",
            cwd: Path::new("/worktree"),
            instructions: "base instructions",
            origin: RolloutOrigin {
                kind: "root",
                parent_thread_id: None,
            },
            resume_history_len: None,
        },
    )
    .expect("create rollout")
}

fn lines(recorder: &RolloutRecorder) -> Vec<Value> {
    BufReader::new(File::open(recorder.info().path()).expect("open rollout"))
        .lines()
        .map(|line| serde_json::from_str(&line.expect("read line")).expect("parse line"))
        .collect()
}

#[test]
fn loads_codex_rollout_without_a_nanocodex_sidecar() {
    let home = tempdir().expect("temporary Codex home");
    let thread_id = "019c0d31-c308-7d91-bff4-5dca82d15ac6";
    let directory = home.path().join("sessions/2026/07/24");
    std::fs::create_dir_all(&directory).expect("create rollout directory");
    let path = directory.join(format!("rollout-2026-07-24T12-00-00-{thread_id}.jsonl"));
    let mut file = File::create(&path).expect("create Codex rollout");
    for value in [
        serde_json::json!({
            "timestamp": "2026-07-24T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": thread_id,
                "cwd": home.path(),
                "originator": "codex-tui",
                "history_mode": "legacy",
                "context_window": {"window_id": "window-1"}
            }
        }),
        serde_json::json!({
            "timestamp": "2026-07-24T12:00:01Z",
            "type": "turn_context",
            "payload": {
                "cwd": home.path(),
                "approval_policy": "on-request",
                "sandbox_policy": {"type": "workspace-write"},
                "model": "gpt-5.5",
                "effort": "high",
                "summary": "auto"
            }
        }),
        serde_json::json!({
            "timestamp": "2026-07-24T12:00:01Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "visible prompt"}
        }),
        serde_json::json!({
            "timestamp": "2026-07-24T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "discarded"}]
            }
        }),
        serde_json::json!({
            "timestamp": "2026-07-24T12:00:02Z",
            "type": "compacted",
            "payload": {
                "replacement_history": [{
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "retained"}]
                }],
                "window_number": 1,
                "first_window_id": "window-1",
                "previous_window_id": "window-1",
                "window_id": "window-2"
            }
        }),
        serde_json::json!({
            "timestamp": "2026-07-24T12:00:03Z",
            "type": "event_msg",
            "payload": {"type": "agent_reasoning", "text": "checking the workspace"}
        }),
        serde_json::json!({
            "timestamp": "2026-07-24T12:00:03Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "visible answer"}
        }),
        serde_json::json!({
            "timestamp": "2026-07-24T12:00:03Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "continued"}]
            }
        }),
    ] {
        write_line(&mut file, &value).expect("write rollout line");
    }
    file.flush().expect("flush rollout");

    let session = RolloutConfig::new(home.path())
        .load_session(thread_id)
        .expect("load Codex rollout");
    assert_eq!(
        Path::new(session.workspace()).canonicalize().unwrap(),
        home.path().canonicalize().unwrap()
    );
    assert_eq!(session.rollout_path(), path.canonicalize().unwrap());
    assert_eq!(session.model(), Model::Sol);
    let snapshot = serde_json::to_value(session.snapshot()).expect("encode snapshot");
    assert_eq!(snapshot["lineage_id"], thread_id);
    assert_eq!(snapshot["prompt_cache_key"], thread_id);
    assert!(snapshot.get("request_prefix").is_none());
    assert_eq!(snapshot["history"].as_array().map(Vec::len), Some(2));
    let history = snapshot["history"].to_string();
    assert!(history.contains("retained"));
    assert!(history.contains("continued"));
    assert!(!history.contains("discarded"));
    assert_eq!(
        session.transcript(),
        [
            RolloutTranscriptItem::User("visible prompt".to_owned()),
            RolloutTranscriptItem::Reasoning("checking the workspace".to_owned()),
            RolloutTranscriptItem::Assistant("visible answer".to_owned()),
        ]
    );
}

#[test]
fn loads_the_latest_supported_model_from_codex_turn_context() {
    let home = tempdir().expect("temporary Codex home");
    let thread_id = "019c0d31-c308-7d91-bff4-5dca82d15ac6";
    let directory = home.path().join("sessions/2026/07/24");
    std::fs::create_dir_all(&directory).expect("create rollout directory");
    let path = directory.join(format!("rollout-2026-07-24T12-00-00-{thread_id}.jsonl"));
    let mut file = File::create(&path).expect("create Nanocodex rollout");
    for value in [
        serde_json::json!({
            "timestamp": "2026-07-24T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": thread_id,
                "cwd": home.path(),
                "history_mode": "legacy",
                "context_window": {"window_id": "window-1"}
            }
        }),
        serde_json::json!({
            "timestamp": "2026-07-24T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "use Luna"}]
            }
        }),
        serde_json::json!({
            "timestamp": "2026-07-24T12:00:02Z",
            "type": "turn_context",
            "payload": {
                "cwd": home.path(),
                "approval_policy": "on-request",
                "sandbox_policy": {"type": "workspace-write"},
                "model": "gpt-5.6-sol",
                "effort": "high",
                "summary": "auto"
            }
        }),
        serde_json::json!({
            "timestamp": "2026-07-24T12:00:03Z",
            "type": "turn_context",
            "payload": {
                "cwd": home.path(),
                "approval_policy": "on-request",
                "sandbox_policy": {"type": "workspace-write"},
                "model": "gpt-5.6-luna",
                "effort": "high",
                "summary": "auto"
            }
        }),
    ] {
        write_line(&mut file, &value).expect("write rollout line");
    }
    file.flush().expect("flush rollout");

    let session = RolloutConfig::new(home.path())
        .load_session(thread_id)
        .expect("load Nanocodex rollout");
    let snapshot = serde_json::to_value(session.snapshot()).expect("encode snapshot");

    assert_eq!(session.model(), Model::Luna);
    assert_eq!(snapshot["model"], "gpt-5.6-luna");
}

#[test]
fn rejects_rollouts_with_paginated_history() {
    let home = tempdir().expect("temporary Codex home");
    let thread_id = "019c0d31-c308-7d91-bff4-5dca82d15ac6";
    let directory = home.path().join("sessions/2026/07/24");
    std::fs::create_dir_all(&directory).expect("create rollout directory");
    let path = directory.join(format!("rollout-2026-07-24T12-00-00-{thread_id}.jsonl"));
    write_line(
        &mut File::create(&path).expect("create Codex rollout"),
        &serde_json::json!({
            "timestamp": "2026-07-24T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": thread_id,
                "cwd": home.path(),
                "history_mode": "paginated",
                "context_window": {"window_id": "window-1"}
            }
        }),
    )
    .expect("write session metadata");

    let error = RolloutConfig::new(home.path())
        .load_session(thread_id)
        .expect_err("paginated rollout must be rejected");
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert!(error.to_string().contains("paginated"));
}

#[test]
fn reconstructs_custom_function_and_mcp_tool_activity() {
    assert_eq!(
        visible_tool_call(&serde_json::json!({
            "type": "custom_tool_call",
            "call_id": "custom-1",
            "name": "exec",
            "input": "text(true);"
        })),
        Some(RolloutTranscriptItem::Tool {
            call_id: "custom-1".to_owned(),
            name: "exec".to_owned(),
            arguments: "text(true);".to_owned(),
        })
    );
    assert_eq!(
        visible_tool_call(&serde_json::json!({
            "type": "function_call",
            "call_id": "function-1",
            "name": "wait",
            "arguments": "{\"cell_id\":\"1\"}"
        })),
        Some(RolloutTranscriptItem::Tool {
            call_id: "function-1".to_owned(),
            name: "wait".to_owned(),
            arguments: "{\"cell_id\":\"1\"}".to_owned(),
        })
    );
    assert_eq!(
        visible_rollout_event(&serde_json::json!({
            "type": "mcp_tool_call_end",
            "call_id": "mcp-1",
            "invocation": {
                "server": "node_repl",
                "tool": "js",
                "arguments": {"code": "return true"}
            }
        })),
        Some(RolloutTranscriptItem::Tool {
            call_id: "mcp-1".to_owned(),
            name: "node_repl.js".to_owned(),
            arguments: "{\"code\":\"return true\"}".to_owned(),
        })
    );
    let web_search = visible_rollout_event(&serde_json::json!({
        "type": "web_search_end",
        "call_id": "search-1",
        "query": "Nanocodex",
        "action": {"type": "search", "queries": ["Nanocodex"]}
    }))
    .expect("web search activity");
    let RolloutTranscriptItem::Tool {
        call_id,
        name,
        arguments,
    } = web_search
    else {
        panic!("web search must reconstruct as tool activity");
    };
    assert_eq!(call_id, "search-1");
    assert_eq!(name, "web_search");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&arguments).expect("web search arguments"),
        serde_json::json!({"type": "search", "queries": ["Nanocodex"]})
    );
}

#[tokio::test]
async fn writes_codex_rollout_envelope_and_committed_items() {
    let home = tempdir().expect("temporary Codex home");
    let recorder = recorder(home.path());
    recorder
        .persist_history(
            ResponseHistory::new(vec![message("remember amber")]),
            0,
            completed_turn("remember amber", "stored"),
        )
        .await
        .expect("persist rollout");

    let lines = lines(&recorder);
    assert_eq!(lines.len(), 8);
    assert_eq!(lines[0]["type"], "session_meta");
    assert_eq!(
        lines[0]["payload"]["id"],
        "019c0d31-c308-7d91-bff4-5dca82d15ac6"
    );
    assert_eq!(lines[0]["payload"]["source"], "cli");
    assert_eq!(lines[0]["payload"]["history_mode"], "legacy");
    assert!(lines[0]["payload"]["context_window"]["window_id"].is_string());
    assert_eq!(lines[1]["type"], "event_msg");
    assert_eq!(lines[1]["payload"]["type"], "task_started");
    assert_eq!(lines[2]["payload"]["type"], "user_message");
    assert_eq!(lines[2]["payload"]["message"], "remember amber");
    assert_eq!(lines[3]["type"], "turn_context");
    assert_eq!(lines[3]["payload"]["cwd"], "/worktree");
    assert_eq!(lines[3]["payload"]["model"], "gpt-5.6-sol");
    assert_eq!(lines[3]["payload"]["effort"], "high");
    assert_eq!(lines[4]["type"], "response_item");
    assert_eq!(lines[4]["payload"]["type"], "message");
    assert_eq!(lines[4]["payload"]["role"], "user");
    assert_eq!(lines[5]["type"], "world_state");
    assert!(
        lines[5]["payload"]["state"]
            .get("nanocodex_model")
            .is_none()
    );
    assert_eq!(
        lines[5]["payload"]["state"]["nanocodex_context"]["kind"],
        "missing"
    );
    assert_eq!(lines[6]["payload"]["type"], "agent_message");
    assert_eq!(lines[6]["payload"]["message"], "stored");
    assert_eq!(lines[6]["payload"]["phase"], "final_answer");
    assert_eq!(lines[7]["payload"]["type"], "task_complete");
    assert_eq!(
        lines[7]["payload"]["turn_id"],
        lines[1]["payload"]["turn_id"]
    );
}

#[tokio::test]
async fn appends_only_the_new_committed_delta() {
    let home = tempdir().expect("temporary Codex home");
    let recorder = recorder(home.path());
    recorder
        .persist_history(
            ResponseHistory::new(vec![message("one")]),
            0,
            completed_turn("one", "first"),
        )
        .await
        .expect("persist first turn");
    let mut prefix = Vec::new();
    File::open(recorder.info().path())
        .expect("open rollout")
        .read_to_end(&mut prefix)
        .expect("read prefix");

    recorder
        .persist_history(
            ResponseHistory::new(vec![message("one"), message("two")]),
            0,
            completed_turn_with_effort("two", "second", Thinking::Low),
        )
        .await
        .expect("persist second turn");

    let mut complete = Vec::new();
    File::open(recorder.info().path())
        .expect("open rollout")
        .read_to_end(&mut complete)
        .expect("read complete rollout");
    assert!(complete.starts_with(&prefix));
    let lines = lines(&recorder);
    assert_eq!(lines.len(), 14);
    assert_eq!(lines[9]["payload"]["message"], "two");
    assert_eq!(lines[10]["type"], "turn_context");
    assert_eq!(lines[10]["payload"]["effort"], "low");
    assert_eq!(lines[11]["type"], "response_item");
    assert_eq!(lines[12]["payload"]["message"], "second");
    assert_eq!(
        lines
            .iter()
            .filter(|line| line["type"] == "world_state")
            .count(),
        1,
        "an unchanged context baseline must not add rollout churn"
    );
}

#[tokio::test]
async fn resumed_writer_repairs_a_rollout_behind_the_durable_boundary() {
    let home = tempdir().expect("temporary Codex home");
    let original = recorder(home.path());
    original
        .persist_history(
            ResponseHistory::new(vec![message("one")]),
            0,
            completed_turn("one", "first"),
        )
        .await
        .expect("persist first turn");
    original.flush().await.expect("flush first turn");
    let path = original.info().path().to_path_buf();
    original
        .shutdown()
        .await
        .expect("close the original rollout writer");

    let config = RolloutConfig::new(home.path()).resumed(path.clone());
    let resumed = RolloutRecorder::create(
        &Handle::current(),
        RolloutCreate {
            config: &config,
            thread_id: "019c0d31-c308-7d91-bff4-5dca82d15ac6",
            prompt_cache_key: "durable-cache",
            cwd: Path::new("/worktree"),
            instructions: "base instructions",
            origin: RolloutOrigin {
                kind: "resume",
                parent_thread_id: None,
            },
            // The durable snapshot already contains `two`, but its rollout append failed.
            resume_history_len: Some(2),
        },
    )
    .expect("resume rollout");
    resumed
        .persist_history(
            ResponseHistory::new(vec![message("one"), message("two"), message("three")]),
            0,
            completed_turn("three", "resumed"),
        )
        .await
        .expect("persist resumed turn");

    let lines = lines(&resumed);
    assert_eq!(
        lines
            .iter()
            .filter(|line| line["type"] == "session_meta")
            .count(),
        1
    );
    let response_text = lines
        .iter()
        .filter(|line| line["type"] == "response_item")
        .map(|line| line["payload"]["content"][0]["text"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(response_text, ["one", "two", "three"]);
}

#[tokio::test]
async fn records_compaction_as_a_replacement_history_boundary() {
    let home = tempdir().expect("temporary Codex home");
    let recorder = recorder(home.path());
    recorder
        .persist_history(
            ResponseHistory::new(vec![message("one"), message("two")]),
            0,
            completed_turn("two", "before compaction"),
        )
        .await
        .expect("persist original history");
    recorder
        .persist_history(
            ResponseHistory::new(vec![message("summary")]),
            1,
            completed_turn("continue", "after compaction"),
        )
        .await
        .expect("persist compaction");
    recorder.flush().await.expect("flush rollout");

    let lines = lines(&recorder);
    let compacted = lines
        .iter()
        .find(|line| line["type"] == "compacted")
        .expect("compacted record");
    assert_eq!(lines.len(), 16);
    assert_eq!(compacted["payload"]["window_number"], 1);
    assert_eq!(
        compacted["payload"]["replacement_history"][0]["content"][0]["text"],
        "summary"
    );
}

#[tokio::test]
async fn fork_metadata_retains_parent_identity() {
    let home = tempdir().expect("temporary Codex home");
    let parent = "019c0d31-c308-7d91-bff4-5dca82d15ac5";
    let recorder = RolloutRecorder::create(
        &Handle::current(),
        RolloutCreate {
            config: &RolloutConfig::new(home.path()),
            thread_id: "019c0d31-c308-7d91-bff4-5dca82d15ac6",
            prompt_cache_key: "durable-cache",
            cwd: Path::new("/worktree"),
            instructions: "base instructions",
            origin: RolloutOrigin {
                kind: "fork",
                parent_thread_id: Some(parent),
            },
            resume_history_len: None,
        },
    )
    .expect("create fork rollout");

    let lines = lines(&recorder);
    assert_eq!(lines[0]["payload"]["forked_from_id"], parent);
    assert_eq!(lines[0]["payload"]["parent_thread_id"], parent);
}

#[tokio::test]
async fn failed_append_remains_pending_and_retries_without_duplicates() {
    let home = tempdir().expect("temporary rollout directory");
    let path = home.path().join("rollout.jsonl");
    let file = File::create(&path).expect("create temporary rollout");
    let mut writer = RolloutWriter::new(
        tokio::fs::File::from_std(file),
        uuid::Uuid::now_v7().to_string(),
        PathBuf::from("/worktree"),
    );
    writer.pending = Some(RolloutCommit::from_history(
        ResponseHistory::new(vec![message("retry me")]),
        0,
        completed_turn("retry me", "retried"),
    ));
    writer.inject_write_failures(2);

    assert!(writer.persist_pending().await.is_err());
    assert!(writer.pending.is_some());
    writer.flush().await.expect("retry pending append");
    drop(writer);

    let lines = BufReader::new(File::open(path).expect("open retried rollout"))
        .lines()
        .collect::<io::Result<Vec<_>>>()
        .expect("read retried rollout");
    assert_eq!(lines.len(), 7);
}
