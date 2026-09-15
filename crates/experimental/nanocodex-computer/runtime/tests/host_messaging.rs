use serde_json::{Value, json};
use skyre::messaging::Messaging;
use std::{fs, path::PathBuf};
fn fixture() -> (tempfile::TempDir, Messaging) {
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("broker.py");
    fs::write(
        &script,
        r#"import json,sys,pathlib,time
r=json.load(sys.stdin);p=r['params'];method=r['method'];root=pathlib.Path(__file__).parent
with (root/'calls.jsonl').open('a') as f:f.write(json.dumps(r)+'\n')
if method=='resolve_recipients':
 if (root/'slowresolve').exists():time.sleep(0.4)
 recipients=p['recipients']
 if (root/'redirect').exists():recipients=['someone-else@example.test']
 result={'account':p['account'],'recipients':recipients}
elif method=='commit_send':
 if p['body']=='uncertain':print('not json');sys.exit(0)
 result={'message_id':'fixture-message','account':p['account']}
elif method=='send_status':result={'state':'sent','message_id':'fixture-message'}
elif method=='list':result={'items':[{'id':'message-1'}],'next_cursor':'page-2'}
elif method=='count':result={'count':3}
elif method=='attachment':result={'data':'aGk=','mime_type':'text/plain'}
else:result={'id':'message-1','body':'fixture'}
print(json.dumps({'ok':True,'result':result}))
"#,
    )
    .unwrap();
    let mut messaging = Messaging::new();
    messaging
        .configure(
            PathBuf::from("/usr/bin/python3"),
            vec![script.to_string_lossy().into_owned()],
            temp.path().to_path_buf(),
        )
        .unwrap();
    (temp, messaging)
}
fn prepare(m: &mut Messaging, body: &str) -> Value {
    m.execute(
        "messages.prepare",
        &json!({"account":"fixture","recipients":["person@example.test"],"body":body}),
    )
    .unwrap()
}
fn authorize(m: &mut Messaging, p: &Value) {
    m.authorize(
        p["id"].as_str().unwrap(),
        p["digest"].as_str().unwrap(),
        "user",
    )
    .unwrap()
}
fn commit(m: &mut Messaging, p: &Value) -> skyre::Result<Value> {
    m.execute(
        "messages.commit",
        &json!({"plan_id":p["id"],"digest":p["digest"]}),
    )
}
#[test]
fn host_message_review_digest_and_single_commit() {
    let (temp, mut m) = fixture();
    let plan = prepare(&mut m, "Hello fixture");
    assert_eq!(commit(&mut m, &plan).unwrap_err().code, -32003);
    assert!(
        m.execute("messages.authorize", &json!({"plan_id":plan["id"]}))
            .is_err()
    );
    assert!(
        m.authorize(plan["id"].as_str().unwrap(), "changed", "user")
            .is_err()
    );
    authorize(&mut m, &plan);
    assert_eq!(commit(&mut m, &plan).unwrap()["state"], "sent");
    assert!(commit(&mut m, &plan).is_err());
    let calls = fs::read_to_string(temp.path().join("calls.jsonl")).unwrap();
    assert_eq!(
        calls
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|v| v["method"] == "commit_send")
            .count(),
        1
    );
}
#[test]
fn host_message_rechecks_attachments_and_recipient_identity() {
    let (temp, mut m) = fixture();
    fs::write(temp.path().join("attachment.txt"), b"before").unwrap();
    let plan=m.execute("messages.prepare",&json!({"account":"fixture","recipients":["person@example.test"],"body":"hi","attachments":["attachment.txt"]})).unwrap();
    authorize(&mut m, &plan);
    fs::write(temp.path().join("attachment.txt"), b"after").unwrap();
    assert!(
        commit(&mut m, &plan)
            .unwrap_err()
            .message
            .contains("Attachment changed")
    );
    let plan = prepare(&mut m, "Hello");
    authorize(&mut m, &plan);
    fs::write(temp.path().join("redirect"), b"yes").unwrap();
    assert!(
        commit(&mut m, &plan)
            .unwrap_err()
            .message
            .contains("recipients changed")
    );
    assert!(
        !fs::read_to_string(temp.path().join("calls.jsonl"))
            .unwrap()
            .contains("commit_send")
    );
}
#[test]
fn host_message_uncertain_outcome_never_retries_and_can_reconcile() {
    let (_temp, mut m) = fixture();
    let plan = prepare(&mut m, "uncertain");
    authorize(&mut m, &plan);
    assert!(
        commit(&mut m, &plan)
            .unwrap_err()
            .message
            .contains("uncertain")
    );
    assert!(commit(&mut m, &plan).is_err());
    let result = m
        .execute("messages.reconcile", &json!({"plan_id":plan["id"]}))
        .unwrap();
    assert_eq!(result["state"], "sent");
    assert!(commit(&mut m, &plan).is_err());
}
#[test]
fn host_message_expiry_pagination_and_attachment_confinement() {
    let (temp, mut m) = fixture();
    let plan=m.execute("messages.prepare",&json!({"account":"fixture","recipients":["person@example.test"],"body":"expires","expires_in_ms":1})).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    assert!(
        m.authorize(
            plan["id"].as_str().unwrap(),
            plan["digest"].as_str().unwrap(),
            "user"
        )
        .is_err()
    );
    assert_eq!(
        m.execute("messages.list", &json!({"account":"fixture","limit":1}))
            .unwrap()["next_cursor"],
        "page-2"
    );
    assert!(
        m.execute("messages.list", &json!({"account":"fixture","limit":0}))
            .is_err()
    );
    assert_eq!(
        m.execute(
            "messages.attachment",
            &json!({"account":"fixture","message_id":"one","attachment_id":"one"})
        )
        .unwrap()["size"],
        2
    );
    assert!(m.execute("messages.prepare",&json!({"account":"fixture","recipients":["a"],"body":"hi","attachments":["../outside"]})).is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/etc/hosts", temp.path().join("link")).unwrap();
        assert!(
            m.execute(
                "messages.prepare",
                &json!({"account":"fixture","recipients":["a"],"body":"hi","attachments":["link"]})
            )
            .is_err()
        );
    }
}

#[test]
fn host_message_expiry_rechecked_after_broker_and_stderr_hidden() {
    let (temp, mut messaging) = fixture();
    let plan=messaging.execute("messages.prepare",&json!({"account":"fixture","recipients":["person@example.test"],"body":"hi","expires_in_ms":250})).unwrap();
    authorize(&mut messaging, &plan);
    fs::write(temp.path().join("slowresolve"), b"1").unwrap();
    assert!(
        commit(&mut messaging, &plan)
            .unwrap_err()
            .message
            .contains("expired")
    );
    assert!(
        !fs::read_to_string(temp.path().join("calls.jsonl"))
            .unwrap()
            .contains("commit_send")
    );
    let script = temp.path().join("broken.py");
    fs::write(
        &script,
        "import sys\nprint('SENSITIVE_FAKE_TOKEN',file=sys.stderr)\nsys.exit(1)\n",
    )
    .unwrap();
    let mut messaging = Messaging::new();
    messaging
        .configure(
            PathBuf::from("/usr/bin/python3"),
            vec![script.to_string_lossy().into_owned()],
            temp.path().to_path_buf(),
        )
        .unwrap();
    let error = messaging
        .execute("messages.accounts", &json!({}))
        .unwrap_err();
    assert!(!error.message.contains("SENSITIVE_FAKE_TOKEN"));
}
