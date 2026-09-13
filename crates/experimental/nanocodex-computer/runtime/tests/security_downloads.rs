use skyre::security::{Security, SecurityConfig};

#[test]
fn download_policy_checks_both_observed_origins_before_admission() {
    let policy = Security::new(SecurityConfig {
        allowed_origins: vec!["https://page.example".into(), "https://cdn.example".into()],
        denied_origins: vec!["https://denied.example".into()],
        preapproved_download_origins: vec![
            "https://page.example".into(),
            "https://denied.example".into(),
        ],
        ..Default::default()
    })
    .unwrap();
    assert!(
        policy
            .check_download("https://PAGE.example:443/a", "https://page.example/file")
            .is_ok()
    );
    for (source, response) in [
        ("https://page.example", "https://denied.example/file"),
        ("https://denied.example", "https://page.example/file"),
        ("https://page.example", "https://unlisted.example/file"),
        ("https://unlisted.example", "https://page.example/file"),
    ] {
        assert_eq!(
            policy.check_download(source, response).unwrap_err().code,
            -32010
        );
    }
    // Allowlisting a CDN does not grant cross-origin transfer approval.
    assert_eq!(
        policy
            .check_download("https://page.example", "https://cdn.example/file")
            .unwrap_err()
            .code,
        -32011
    );
}

#[test]
fn download_policy_rejects_unknown_origins_and_has_no_implicit_bypass() {
    let policy = Security::default();
    assert!(
        policy
            .check_download("http://localhost:123/a", "http://localhost:123/b")
            .is_err()
    );
    for (source, response) in [
        ("about:blank", "https://page.example/file"),
        ("https://page.example", "about:blank"),
        ("file:///tmp/source", "https://page.example/file"),
        ("https://user@page.example", "https://page.example/file"),
        (
            "https://page.example",
            "https://user:password@page.example/file",
        ),
        ("https://page.example", "blob:https://page.example/id"),
    ] {
        assert!(policy.check_download(source, response).is_err());
    }
    assert_eq!(
        policy
            .check_download("https://page.example", "http://page.example/file")
            .unwrap_err()
            .code,
        -32011
    );
    assert_eq!(
        policy
            .check_download("https://page.example", "https://cdn.example/file")
            .unwrap_err()
            .message,
        "Download review instructions are not configured"
    );
    let configured = Security::new(SecurityConfig {
        review_instructions: "Review this synthetic transfer".into(),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        configured
            .check_download("https://page.example", "https://cdn.example/file")
            .unwrap_err()
            .message,
        "Download reviewer unavailable"
    );
}

#[cfg(unix)]
#[test]
fn cross_origin_download_review_is_host_constructed_and_fail_closed() {
    use serde_json::{Value, json};
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("reviewer");
    std::fs::write(
        &executable,
        "#!/usr/bin/python3\nimport json, pathlib, sys\np=pathlib.Path(__file__)\nx=json.load(sys.stdin)\np.with_suffix('.request.json').write_text(json.dumps(x))\nprint(p.with_suffix('.response.json').read_text())\n",
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let policy = Security::new(SecurityConfig {
        reviewer: Some(executable.to_str().unwrap().into()),
        review_instructions: "Review this synthetic cross-origin download".into(),
        preapproved_download_origins: vec!["https://page.example".into()],
        ..Default::default()
    })
    .unwrap();
    for (response, expected) in [
        (json!({"action":"accept","reviewer":"auto_review"}), None),
        (
            json!({"action":"accept","reviewer":"guardian_subagent"}),
            None,
        ),
        (json!({"action":"accept"}), Some(-32011)),
        (json!({"action":"accept","reviewer":"user"}), Some(-32011)),
        (
            json!({"action":"decline","reviewer":"auto_review"}),
            Some(-32012),
        ),
        (
            json!({"action":"cancel","reviewer":"auto_review"}),
            Some(-32013),
        ),
        (
            json!({"action":"unexpected","reviewer":"auto_review"}),
            Some(-32011),
        ),
    ] {
        policy.clear_download_approvals();
        std::fs::write(
            executable.with_extension("response.json"),
            serde_json::to_vec(&response).unwrap(),
        )
        .unwrap();
        let result = policy.check_download("https://page.example/form", "https://cdn.example/file");
        assert_eq!(result.err().map(|error| error.code), expected);
        let request: Value = serde_json::from_slice(
            &std::fs::read(executable.with_extension("request.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            request,
            json!({
                "type":"review",
                "instructions":"Review this synthetic cross-origin download",
                "context":{
                    "operation":"file_download",
                    "url":"https://cdn.example/file",
                    "origin":"https://cdn.example",
                    "tool_name":"download_browser_files",
                    "file_transfer":"download"
                },
                "metadata":{"automated":true,"sensitive":true,"strict":true}
            })
        );
    }
    // Explicit host preapproval survives transient-grant clearing.
    std::fs::remove_file(executable.with_extension("request.json")).unwrap();
    assert!(
        policy
            .check_download("https://page.example/a", "https://page.example/b")
            .is_ok()
    );
    assert!(!executable.with_extension("request.json").exists());
}

#[cfg(unix)]
#[test]
fn source_then_destination_approval_is_cached_and_revoked_across_clones() {
    use serde_json::Value;
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("reviewer");
    std::fs::write(&executable, "#!/usr/bin/python3\nimport json,pathlib,sys\np=pathlib.Path(__file__)\nx=json.load(sys.stdin)\nwith p.with_suffix('.jsonl').open('a') as f: f.write(json.dumps(x)+'\\n')\nprint(json.dumps({'action':'accept','reviewer':'auto_review'}))\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let policy = Security::new(SecurityConfig {
        reviewer: Some(executable.to_str().unwrap().into()),
        review_instructions: "Review owned synthetic transfers".into(),
        ..Default::default()
    })
    .unwrap();
    let clone = policy.clone();
    let requests = || -> Vec<Value> {
        std::fs::read_to_string(executable.with_extension("jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    };
    policy
        .check_download("https://page.example/form", "https://cdn.example/file")
        .unwrap();
    let recorded = requests();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0]["context"]["origin"], "https://page.example");
    assert_eq!(recorded[1]["context"]["origin"], "https://cdn.example");
    clone
        .check_download("https://cdn.example/another", "https://page.example/file")
        .unwrap();
    assert_eq!(requests().len(), 2);
    policy.clear_download_approvals();
    clone
        .check_download("https://page.example/form", "https://page.example/file")
        .unwrap();
    assert_eq!(requests().len(), 3);
}

#[cfg(unix)]
#[test]
fn ended_download_session_cannot_be_reapproved_by_a_late_reviewer() {
    use std::{
        os::unix::fs::PermissionsExt,
        time::{Duration, Instant},
    };
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("reviewer");
    std::fs::write(&executable, "#!/usr/bin/python3\nimport json,pathlib,sys,time\np=pathlib.Path(__file__)\njson.load(sys.stdin)\np.with_suffix('.ready').touch()\ndeadline=time.monotonic()+5\nwhile not p.with_suffix('.release').exists():\n if time.monotonic()>deadline: sys.exit(1)\n time.sleep(.005)\nprint(json.dumps({'action':'accept','reviewer':'auto_review'}))\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let policy = Security::new(SecurityConfig {
        reviewer: Some(executable.to_str().unwrap().into()),
        review_instructions: "Review owned synthetic transfer".into(),
        ..Default::default()
    })
    .unwrap();
    let clone = policy.clone();
    let worker = std::thread::spawn(move || {
        clone.check_download("https://page.example", "https://page.example/file")
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !executable.with_extension("ready").exists() {
        assert!(
            Instant::now() < deadline,
            "Owned reviewer did not become ready"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    policy.clear_download_approvals();
    std::fs::write(executable.with_extension("release"), []).unwrap();
    assert_eq!(worker.join().unwrap().unwrap_err().code, -32014);
    std::fs::remove_file(executable.with_extension("ready")).unwrap();
    policy
        .check_download("https://page.example", "https://page.example/file")
        .unwrap();
    assert!(
        executable.with_extension("ready").exists(),
        "Late approval must not install a grant in the new session"
    );
}

#[cfg(unix)]
#[test]
fn expired_download_budget_never_starts_a_reviewer_or_uses_a_cached_grant() {
    use skyre::process_rpc::Program;
    use std::{
        os::unix::fs::PermissionsExt,
        time::{Duration, Instant},
    };
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("reviewer");
    let marker = executable.with_extension("invoked");
    std::fs::write(&executable, "#!/usr/bin/python3\nimport json,pathlib,sys\npathlib.Path(__file__).with_suffix('.invoked').touch()\njson.load(sys.stdin)\nprint(json.dumps({'action':'accept','reviewer':'auto_review'}))\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let deadline = Instant::now();
    let program = Program::new(&executable, Duration::from_secs(30)).unwrap();
    assert!(
        program
            .request_until(&serde_json::json!({}), deadline)
            .is_err()
    );
    assert!(!marker.exists());
    let policy = Security::new(SecurityConfig {
        reviewer: Some(executable.to_str().unwrap().into()),
        review_instructions: "Review owned synthetic transfer".into(),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        policy
            .check_download_until(
                "https://page.example",
                "https://page.example/file",
                deadline
            )
            .unwrap_err()
            .code,
        -32008
    );
    assert!(!marker.exists());
    policy
        .check_download_until(
            "https://page.example",
            "https://page.example/file",
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
    std::fs::remove_file(&marker).unwrap();
    assert_eq!(
        policy
            .check_download_until(
                "https://page.example",
                "https://page.example/file",
                deadline
            )
            .unwrap_err()
            .code,
        -32008
    );
    assert!(!marker.exists());
}

#[cfg(unix)]
#[test]
fn download_budget_bounds_slow_source_review_and_prevents_destination_review() {
    use serde_json::Value;
    use std::{
        os::unix::fs::PermissionsExt,
        time::{Duration, Instant},
    };
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("reviewer");
    // The operation budget includes executable startup. Leave enough time for
    // a cold shell launch before exercising the ten-second review timeout.
    // The shell records the exact JSON request before sleep.
    std::fs::write(
        &executable,
        r#"#!/bin/sh
IFS= read -r request || :
printf '%s\n' "$request" >> "$0.jsonl"
sleep 10
printf '%s\n' '{"action":"accept","reviewer":"auto_review"}'
"#,
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let policy = Security::new(SecurityConfig {
        reviewer: Some(executable.to_str().unwrap().into()),
        review_instructions: "Review owned synthetic transfer".into(),
        ..Default::default()
    })
    .unwrap();
    let start = Instant::now();
    let failure = policy
        .check_download_until(
            "https://page.example",
            "https://cdn.example/file",
            start + Duration::from_secs(2),
        )
        .unwrap_err();
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "The ten-second reviewer escaped the inherited operation budget"
    );
    let recorded: Vec<Value> = std::fs::read_to_string(executable.with_extension("jsonl"))
        .unwrap_or_else(|error| {
            panic!("Reviewer produced no request record: {error}; admission failed with {failure}")
        })
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["context"]["origin"], "https://page.example");
}

#[test]
fn human_download_prompt_matches_original_session_scope_and_orders_both_origins() {
    use serde_json::{Value, json};
    use skyre::{Result, security::DownloadApproval};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    struct Approval(Arc<Mutex<Vec<Value>>>);
    impl DownloadApproval for Approval {
        fn request(&self, request: Value, deadline: Option<Instant>) -> Result<Value> {
            assert!(deadline.unwrap() > Instant::now());
            self.0.lock().unwrap().push(request);
            Ok(json!({"action":"accept","_meta":{"persist":"session"}}))
        }
    }
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut policy = Security::default();
    policy.set_download_approval(Some(Arc::new(Approval(requests.clone()))));
    let deadline = Instant::now() + Duration::from_secs(3);
    policy
        .check_download_until(
            "https://page.example/form",
            "https://cdn.example/file",
            deadline,
        )
        .unwrap();
    let records = requests.lock().unwrap().clone();
    assert_eq!(records.len(), 2);
    assert_eq!(
        records[0],
        json!({
            "message":"Allow download from https://page.example/form?",
            "meta":{"codex_approval_kind":"mcp_tool_call", "connector_id":"browser-use",
                "connector_name":"Browser Use", "persist":"session", "tool_name":"download_browser_files",
                "tool_title":"Download browser files", "tool_params":{"origin":"https://page.example"},
                "file_transfer":"download", "origin":"https://page.example"}
        })
    );
    assert_eq!(records[1]["meta"]["origin"], "https://cdn.example");
    policy
        .clone()
        .check_download_until(
            "https://page.example/next",
            "https://cdn.example/next",
            deadline,
        )
        .unwrap();
    assert_eq!(requests.lock().unwrap().len(), 2);
    policy.clear_download_approvals();
    policy
        .check_download_until(
            "https://page.example/form",
            "https://cdn.example/file",
            deadline,
        )
        .unwrap();
    assert_eq!(requests.lock().unwrap().len(), 4);
}

#[test]
fn human_download_decisions_cannot_grant_after_deadline_or_connection_change() {
    use serde_json::{Value, json};
    use skyre::{Result, security::DownloadApproval};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    struct Approval {
        response: Value,
        ended: Option<Security>,
        delay: bool,
    }
    impl DownloadApproval for Approval {
        fn request(&self, _: Value, _: Option<Instant>) -> Result<Value> {
            if let Some(policy) = &self.ended {
                policy.clear_download_approvals();
            }
            if self.delay {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(self.response.clone())
        }
    }
    for (response, expected) in [
        (json!({"action":"decline"}), -32012),
        (json!({"action":"cancel"}), -32013),
        (json!({"action":"other"}), -32011),
    ] {
        let mut policy = Security::default();
        policy.set_download_approval(Some(Arc::new(Approval {
            response,
            ended: None,
            delay: false,
        })));
        assert_eq!(
            policy
                .check_download("https://page.example/a", "https://cdn.example/b")
                .unwrap_err()
                .code,
            expected
        );
        policy.set_download_approval(None);
        assert!(
            policy
                .check_download("https://page.example/a", "https://page.example/b")
                .is_err()
        );
    }
    let mut policy = Security::default();
    let ended = policy.clone();
    policy.set_download_approval(Some(Arc::new(Approval {
        response: json!({"action":"accept"}),
        ended: Some(ended),
        delay: false,
    })));
    assert_eq!(
        policy
            .check_download("https://page.example/a", "https://page.example/b")
            .unwrap_err()
            .code,
        -32014
    );
    policy.set_download_approval(Some(Arc::new(Approval {
        response: json!({"action":"accept"}),
        ended: None,
        delay: true,
    })));
    assert_eq!(
        policy
            .check_download_until(
                "https://page.example/a",
                "https://page.example/b",
                Instant::now() + Duration::from_millis(5)
            )
            .unwrap_err()
            .code,
        -32008
    );
    policy.set_download_approval(None);
    assert!(
        policy
            .check_download("https://page.example/a", "https://page.example/b")
            .is_err()
    );
}

#[test]
fn human_transfer_persistence_is_explicit_and_scoped_to_conversation_or_subagent() {
    use serde_json::{Value, json};
    use skyre::{Result, security::DownloadApproval};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;
    struct Approval(Arc<Mutex<(Value, usize)>>);
    impl DownloadApproval for Approval {
        fn request(&self, _: Value, _: Option<Instant>) -> Result<Value> {
            let mut state = self.0.lock().unwrap();
            state.1 += 1;
            Ok(state.0.clone())
        }
    }
    let state = Arc::new(Mutex::new((
        json!({"action":"accept","_meta":{"persist":"session"}}),
        0,
    )));
    let mut policy = Security::default();
    policy.set_download_scope("parent");
    policy.set_download_approval(Some(Arc::new(Approval(state.clone()))));
    let parent = policy.clone();
    let check = |policy: &Security| {
        policy.check_download("https://owned.example/a", "https://owned.example/b")
    };
    check(&parent).unwrap();
    policy.set_download_scope("child");
    check(&policy).unwrap();
    check(&parent).unwrap();
    assert_eq!(state.lock().unwrap().1, 2);
    policy.set_download_scope("second-child");
    state.lock().unwrap().0 = json!({"action":"decline","content":{"persist":"session"}});
    assert_eq!(check(&policy).unwrap_err().code, -32012);
    assert_eq!(check(&policy).unwrap_err().code, -32012);
    assert_eq!(state.lock().unwrap().1, 3);
    check(&parent).unwrap();
    policy.clear_download_approvals();
    assert_eq!(check(&parent).unwrap_err().code, -32012);
    assert_eq!(state.lock().unwrap().1, 4);
    for (response, expected_requests) in [
        (json!({"action":"accept"}), 2),
        (json!({"action":"accept","_meta":{"persist":"always"}}), 2),
        (
            json!({"action":"accept","_meta":{"persist":"always"},"content":{"persist":"session"}}),
            2,
        ),
        (
            json!({"action":"accept","_meta":{"persist":"session"},"content":{"persist":"always"}}),
            1,
        ),
        (
            json!({"action":"accept","_meta":{"persist":"invalid"},"content":{"persist":"session"}}),
            1,
        ),
        (json!({"action":"decline"}), 2),
        (
            json!({"action":"decline","_meta":{"persist":"session","approvals_reviewer":"auto_review"}}),
            2,
        ),
    ] {
        policy.clear_download_approvals();
        *state.lock().unwrap() = (response.clone(), 0);
        for _ in 0..2 {
            assert_eq!(check(&policy).is_ok(), response["action"] == "accept");
        }
        assert_eq!(state.lock().unwrap().1, expected_requests, "{response}");
    }
}
