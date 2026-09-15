use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use skyre::host_services::Services;
use tempfile::TempDir;
fn setup() -> (TempDir, Services) {
    let temp = tempfile::tempdir().unwrap();
    let services = Services::open(temp.path().join("host"), "session-a").unwrap();
    (temp, services)
}
fn call(s: &mut Services, m: &str, p: Value) -> Value {
    s.execute(m, &p).unwrap()
}
#[test]
fn host_history_is_opt_in_owned_retained_and_persistent() {
    let (temp, mut services) = setup();
    services
        .record("action", json!({"value":"not persisted"}))
        .unwrap();
    assert_eq!(
        call(&mut services, "host.history.list", json!({}))["records"],
        json!([])
    );
    call(
        &mut services,
        "host.configuration.patch",
        json!({"patch":{"retention":{"max_records":2}}}),
    );
    call(
        &mut services,
        "host.history.start",
        json!({"include_payload":true}),
    );
    for i in 0..3 {
        services
            .record("action", json!({"n":i,"password":"hidden"}))
            .unwrap();
    }
    let rows = call(&mut services, "host.history.list", json!({}));
    assert_eq!(rows["records"].as_array().unwrap().len(), 2);
    assert_eq!(rows["records"][0]["payload"]["n"], 1);
    assert_eq!(rows["records"][0]["payload"]["password"], "[REDACTED]");
    call(&mut services, "host.history.pause", json!({}));
    services.record("action", json!({"n":4})).unwrap();
    assert!(
        services
            .execute("host.history.append", &json!({"kind":"action"}))
            .is_err()
    );
    let mut other = Services::open(temp.path().join("host"), "session-b").unwrap();
    assert!(other.execute("host.history.stop", &json!({})).is_err());
    assert_eq!(
        call(&mut other, "host.history.list", json!({}))["records"],
        json!([])
    );
    assert_eq!(
        call(&mut other, "host.history.clear", json!({}))["removed"],
        0
    );
    drop(services);
    let mut reopened = Services::open(temp.path().join("host"), "session-a").unwrap();
    assert_eq!(
        call(&mut reopened, "host.history.list", json!({}))["records"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    call(&mut reopened, "host.history.resume", json!({}));
    reopened.end_session().unwrap();
    assert_eq!(
        call(&mut reopened, "host.history.status", json!({}))["running"],
        false
    );
}
#[test]
fn host_configuration_forced_merge_cas_and_policy_snapshot() {
    let (temp, mut a) = setup();
    let mut b = Services::open(temp.path().join("host"), "session-b").unwrap();
    assert_eq!(
        call(&mut a, "host.configuration.read", json!({}))["revision"],
        0
    );
    call(
        &mut b,
        "host.configuration.patch",
        json!({"expected_revision":0,"patch":{"applications":{"fixture":"allowed"}}}),
    );
    assert_eq!(
        call(&mut a, "host.configuration.read", json!({}))["revision"],
        0
    );
    assert_eq!(
        call(&mut a, "host.policy.snapshot", json!({"app":"fixture"}))["access"],
        "allowed"
    );
    assert!(
        a.execute(
            "host.configuration.patch",
            &json!({"expected_revision":0,"patch":{"allow_locked_computer":true}})
        )
        .is_err()
    );
    let updated = call(
        &mut a,
        "host.configuration.patch",
        json!({"expected_revision":1,"patch":{"diagnostics_enabled":true}}),
    );
    assert_eq!(updated["applications"]["fixture"], "allowed");
    assert!(
        a.execute(
            "host.configuration.patch",
            &json!({"patch":{"arbitrary":true}})
        )
        .is_err()
    );
    assert!(
        a.execute(
            "host.configuration.patch",
            &json!({"patch":{"retention":{"max_records":0}}})
        )
        .is_err()
    );
    assert!(
        a.execute(
            "host.configuration.patch",
            &json!({"patch":{"revision":999}})
        )
        .is_err()
    );
    assert_eq!(
        call(&mut a, "host.policy.snapshot", json!({"app":"missing"}))["access"],
        "denied"
    );
}
#[test]
fn host_diagnostics_redact_nested_structured_secrets_and_require_opt_in() {
    let (_temp, mut services) = setup();
    let payload = json!({"kind":"transport.error","payload":{"authorization":"bearer fake","details":[{"api_key":"abc","status":500}]}});
    assert!(
        services
            .execute("host.diagnostics.write", &payload)
            .is_err()
    );
    call(
        &mut services,
        "host.configuration.patch",
        json!({"patch":{"diagnostics_enabled":true}}),
    );
    call(&mut services, "host.diagnostics.write", payload);
    let row = call(&mut services, "host.diagnostics.list", json!({}))["records"][0].clone();
    assert_eq!(row["payload"]["authorization"], "[REDACTED]");
    assert_eq!(row["payload"]["details"][0]["api_key"], "[REDACTED]");
    assert_eq!(row["payload"]["details"][0]["status"], 500);
    assert_eq!(
        call(&mut services, "host.diagnostics.clear", json!({}))["removed"],
        1
    );
}
#[test]
fn host_event_cursors_report_loss_filter_and_do_not_replay() {
    let (_temp, mut services) = setup();
    let sub = call(
        &mut services,
        "host.events.subscribe",
        json!({"after":0,"kinds":["wanted"]}),
    )["subscription"]
        .clone();
    for i in 0..1005 {
        services
            .record(if i % 2 == 0 { "wanted" } else { "other" }, json!({"i":i}))
            .unwrap();
    }
    let first = call(
        &mut services,
        "host.events.poll",
        json!({"subscription":sub,"limit":2}),
    );
    assert_eq!(first["lost"], 5);
    assert_eq!(first["events"][0]["payload"]["i"], 6);
    let cursor = first["cursor"].as_u64().unwrap();
    let next = call(
        &mut services,
        "host.events.poll",
        json!({"subscription":sub,"limit":1000}),
    );
    assert!(next["events"][0]["id"].as_u64().unwrap() > cursor);
    assert_eq!(
        call(
            &mut services,
            "host.events.poll",
            json!({"subscription":sub})
        )["events"],
        json!([])
    );
    call(
        &mut services,
        "host.events.unsubscribe",
        json!({"subscription":sub}),
    );
    assert!(
        services
            .execute("host.events.poll", &json!({"subscription":sub}))
            .is_err()
    );
}
fn install() -> Value {
    let content = b"Independent fixture documentation";
    let digest: String = Sha256::digest(content)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    json!({"manifest":{"id":"fixture","version":"1.0.0","files":{"skills/SKILL.md":digest},"entrypoints":{"skill":"skills/SKILL.md"}},"files":{"skills/SKILL.md":STANDARD.encode(content)}})
}
#[test]
fn host_plugins_integrity_versions_and_lifecycle() {
    let (temp, mut services) = setup();
    let payload = install();
    call(&mut services, "host.plugins.install", payload.clone());
    assert!(services.execute("host.plugins.install", &payload).is_err());
    assert!(
        services
            .execute("host.plugins.resolve", &json!({"id":"fixture"}))
            .is_err()
    );
    call(
        &mut services,
        "host.plugins.activate",
        json!({"id":"fixture","version":"1.0.0"}),
    );
    let resolved = call(
        &mut services,
        "host.plugins.resolve",
        json!({"id":"fixture"}),
    );
    assert_eq!(
        std::fs::read_to_string(resolved["entrypoints"]["skill"].as_str().unwrap()).unwrap(),
        "Independent fixture documentation"
    );
    std::fs::write(
        temp.path()
            .join("host/plugins/fixture/1.0.0/skills/SKILL.md"),
        b"modified",
    )
    .unwrap();
    assert!(
        services
            .execute("host.plugins.resolve", &json!({"id":"fixture"}))
            .is_err()
    );
    call(
        &mut services,
        "host.plugins.uninstall",
        json!({"id":"fixture","version":"1.0.0"}),
    );
    assert!(!temp.path().join("host/plugins/fixture/1.0.0").exists());
    assert_eq!(
        call(&mut services, "host.plugins.list", json!({})),
        json!({})
    );
}
#[test]
fn host_plugins_reject_traversal_bad_checksums_and_unknown_assets() {
    let (_temp, mut services) = setup();
    let mut bad = install();
    bad["manifest"]["id"] = json!("../escape");
    assert!(services.execute("host.plugins.install", &bad).is_err());
    let mut bad = install();
    bad["manifest"]["files"]["skills/SKILL.md"] = json!("bad");
    assert!(services.execute("host.plugins.install", &bad).is_err());
    let mut bad = install();
    bad["files"]["extra"] = json!("");
    assert!(services.execute("host.plugins.install", &bad).is_err());
    let mut bad = install();
    bad["manifest"]["entrypoints"]["skill"] = json!("../escape");
    assert!(services.execute("host.plugins.install", &bad).is_err());
}
#[cfg(unix)]
#[test]
fn host_store_rejects_symlink_world_access_and_existing_writer() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let (temp, mut services) = setup();
    let root = temp.path().join("host");
    symlink(&root, temp.path().join("link")).unwrap();
    assert!(Services::open(temp.path().join("link"), "s").is_err());
    std::fs::set_permissions(
        root.join("state.json"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(services.execute("host.status", &json!({})).is_err());
    std::fs::set_permissions(
        root.join("state.json"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    std::fs::write(root.join(".write-lock"), b"fixture lock").unwrap();
    assert_eq!(
        services
            .execute("host.configuration.patch", &json!({"patch":{}}))
            .unwrap_err()
            .code,
        -32009
    );
}
