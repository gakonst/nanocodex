//! External MCP transport contract tests. No custom CUA runtime is used.
#![cfg(unix)]

use nanocodex_computer::{ComputerConfig, ComputerTools};
use nanocodex_oai_api::tools::{Tool, ToolContext, ToolInput, ToolOutputBody, ToolOutputContent};
use serde_json::{Value, json};
use std::time::Duration;

fn catalog() -> Value {
    json!([
        {"name":"js", "description":"Exact upstream documentation\nincluding whitespace.", "inputSchema":{"type":"object","additionalProperties":true}, "annotations":{"readOnlyHint":false}, "outputSchema":{"type":"object"}},
        {"name":"js_reset", "description":"Upstream reset", "inputSchema":{"type":"object"}},
        {"name":"future_tool", "description":"Provider decides argument meanings", "inputSchema":{"type":"object"}, "_meta":{"custom":[1,2]}},
        {"name":"turn_ended", "inputSchema":{"type":"object"}, "_meta":{"ui":{"visibility":[]}}}
    ])
}

fn config() -> ComputerConfig {
    // Reject any appended companion/platform flags, and echo raw wire requests.
    let script = format!(
        r#"
[ "$#" -eq 0 ] || exit 64
IFS= read -r initialize
printf '%s\n' '{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"2025-06-18","capabilities":{{}}}}}}'
IFS= read -r initialized
IFS= read -r list
printf '%s\n' '{{"jsonrpc":"2.0","id":2,"result":{{"tools":{catalog}}}}}'
next=3
count=0
while IFS= read -r call; do
    case "$call" in
        *'"code":"wait"'*) IFS= read -r never; exit 0 ;;
        *'"code":"slow"'*) sleep 0.3; count=$((count + 1)) ;;
        *'"code":"fail-reset"'*)
            printf '{{"jsonrpc":"2.0","id":%s,"result":{{"content":[],"isError":true}}}}\n' "$next"
            next=$((next + 1))
            continue ;;
        *'"name":"js_reset"'*) count=0 ;;
        *) count=$((count + 1)) ;;
    esac
    printf '{{"jsonrpc":"2.0","id":%s,"result":{{"content":[],"structuredContent":{{"count":%s,"call":%s,"initialize":%s}}}}}}\n' "$next" "$count" "$call" "$initialize"
    next=$((next + 1))
done
"#,
        catalog = catalog()
    );
    let mut config = ComputerConfig::new("/bin/sh");
    config.args = vec!["-c".into(), script.into()];
    config
}

fn context(session: &str) -> ToolContext<'_> {
    ToolContext::new("fixture-model", session, "fixture-call", &[], 16000)
}
fn input(arguments: Value) -> ToolInput {
    ToolInput::Function(serde_json::value::to_raw_value(&arguments).unwrap())
}

#[tokio::test]
async fn external_catalog_arguments_and_metadata_cross_the_process_unchanged() {
    let computer = ComputerTools::connect(config()).await.unwrap();
    assert_eq!(serde_json::to_value(computer.catalog()).unwrap(), catalog());
    assert_eq!(computer.tools().count(), 3);
    assert!(computer.tool("turn_ended").is_some());
    let definition = serde_json::to_value(computer.js().definition()).unwrap();
    assert_eq!(definition["name"], "mcp__cua_repl__js");
    assert_eq!(definition["description"], catalog()[0]["description"]);
    assert_eq!(
        computer.js().provider_definition().input_schema,
        catalog()[0]["inputSchema"]
    );
    for name in ["js", "future_tool", "turn_ended"] {
        let arguments =
            json!({"code":"anything", "timeout_ms":"provider-owned", "unknown":{"value":true}});
        let result = computer
            .tool(name)
            .unwrap()
            .execute(input(arguments.clone()), context("one"))
            .await
            .unwrap();
        let wire = &result.structured_result()["structuredContent"]["call"];
        assert_eq!(wire["params"]["arguments"], arguments);
        assert_eq!(wire["params"]["name"], name);
        assert_eq!(
            wire["params"]["_meta"]["x-codex-turn-metadata"],
            json!({"session_id":"one", "thread_id":"one", "call_id":"fixture-call", "model":"fixture-model"})
        );
    }
}

#[tokio::test]
async fn conversations_retain_independent_upstream_processes_and_reset() {
    let computer = ComputerTools::connect(config()).await.unwrap();
    for (session, count) in [("one", 1), ("one", 2), ("two", 1)] {
        let result = computer
            .js()
            .execute(input(json!({})), context(session))
            .await
            .unwrap();
        assert_eq!(
            result.structured_result()["structuredContent"]["count"],
            count
        );
    }
    computer
        .reset()
        .execute(input(json!({})), context("one"))
        .await
        .unwrap();
    let result = computer
        .js()
        .execute(input(json!({})), context("one"))
        .await
        .unwrap();
    assert_eq!(result.structured_result()["structuredContent"]["count"], 1);
}

#[tokio::test]
async fn caller_cancellation_discards_only_its_transport_and_requires_successful_reset() {
    let computer = ComputerTools::connect(config()).await.unwrap();
    computer
        .js()
        .execute(input(json!({})), context("cancelled"))
        .await
        .unwrap();
    let js = computer.js();
    let task = tokio::spawn(async move {
        js.execute(input(json!({"code":"wait"})), context("cancelled"))
            .await
    });
    // The other conversation must complete while the first provider is blocked.
    tokio::time::timeout(
        Duration::from_secs(3),
        computer.js().execute(input(json!({})), context("other")),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    task.abort();
    let _ = task.await;
    let error = computer
        .js()
        .execute(input(json!({})), context("cancelled"))
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("cua_repl.js_reset"), "{error}");
    assert!(error.contains("effects are uncertain"), "{error}");
    assert!(
        error.contains("Reset does not prove earlier input stopped"),
        "{error}"
    );
    // The peer retains its original realm across cancellation of the first.
    let peer = computer
        .js()
        .execute(input(json!({})), context("other"))
        .await
        .unwrap();
    assert_eq!(peer.structured_result()["structuredContent"]["count"], 2);
    let failed_reset = computer
        .reset()
        .execute(input(json!({"code":"fail-reset"})), context("cancelled"))
        .await
        .unwrap();
    assert!(!failed_reset.success);
    let still_interrupted = computer
        .js()
        .execute(input(json!({})), context("cancelled"))
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(
        still_interrupted.contains("cua_repl.js_reset"),
        "{still_interrupted}"
    );
    computer
        .reset()
        .execute(input(json!({})), context("cancelled"))
        .await
        .unwrap();
    assert!(
        computer
            .js()
            .execute(input(json!({})), context("cancelled"))
            .await
            .unwrap()
            .success
    );
}

#[test]
fn image_bytes_determine_the_api_mime_and_invalid_images_fail() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let data = STANDARD.encode([0xff, 0xd8, 0xff, 0xe0, 0, 0, 0xff, 0xd9]);
    let output = nanocodex_computer::output(
        json!({"content":[{"type":"image","mimeType":"image/png","data":data}]}),
    )
    .unwrap();
    let ToolOutputBody::Content(content) = output.output else {
        panic!("Expected image content")
    };
    assert!(
        matches!(&content[0], ToolOutputContent::InputImage { image_url, detail: nanocodex_oai_api::ImageDetail::Original } if image_url.starts_with("data:image/jpeg;base64,"))
    );
    assert!(
        nanocodex_computer::output(
            json!({"content":[{"type":"image","mimeType":"image/png","data":"AAAA"}]})
        )
        .is_err()
    );
}

#[tokio::test]
#[ignore = "requires NANOCODEX_TEST_EXTERNAL_COMPUTER pointing to an installed external MCP launcher"]
async fn installed_external_provider_discovery_preserves_catalog_and_hides_lifecycle_hook() {
    let Some(executable) = std::env::var_os("NANOCODEX_TEST_EXTERNAL_COMPUTER") else {
        eprintln!("Skipping installed-provider smoke: NANOCODEX_TEST_EXTERNAL_COMPUTER is unset");
        return;
    };
    // Discovery sends initialize/tools/list only. This does not claim native UI control.
    let computer = tokio::time::timeout(
        Duration::from_secs(45),
        ComputerTools::connect(ComputerConfig::mcp(executable)),
    )
    .await
    .expect("external provider discovery timed out")
    .expect("external provider MCP discovery failed");
    let names = computer
        .tools()
        .map(|tool| tool.definition().name().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "mcp__cua_repl__js",
            "mcp__cua_repl__js_add_node_module_dir",
            "mcp__cua_repl__js_reset",
        ]
    );
    let hidden = computer
        .tool("turn_ended")
        .expect("trusted lifecycle hook missing from the raw catalog");
    assert!(!hidden.provider_definition().model_visible());
    assert_eq!(
        hidden.provider_definition().metadata["_meta"]["ui"]["visibility"],
        json!([])
    );
    assert_eq!(computer.catalog().len(), 4);
    assert!(computer.catalog().iter().all(|definition| {
        definition.input_schema.is_object()
            && definition
                .description
                .as_deref()
                .is_some_and(|text| !text.is_empty())
    }));
}

fn paginated_config(second_page: Value) -> ComputerConfig {
    let first_page = json!({"tools":[catalog()[0].clone()], "nextCursor":"opaque-next-page"});
    let script = format!(
        r#"
IFS= read -r initialize
printf '%s\n' '{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"2025-06-18","capabilities":{{}}}}}}'
IFS= read -r initialized
IFS= read -r list
printf '%s\n' '{{"jsonrpc":"2.0","id":2,"result":{first_page}}}'
IFS= read -r page
case "$page" in
    *'"cursor":"opaque-next-page"'*) : ;;
    *) exit 64 ;;
esac
printf '%s\n' '{{"jsonrpc":"2.0","id":3,"result":{second_page}}}'
IFS= read -r end
"#
    );
    let mut config = ComputerConfig::mcp("/bin/sh");
    config.args = vec!["-c".into(), script.into()];
    config
}

#[tokio::test]
async fn discovers_all_catalog_pages_without_changing_metadata() {
    let expected = catalog();
    let config = paginated_config(json!({"tools":expected.as_array().unwrap()[1..]}));
    let computer = ComputerTools::connect(config).await.unwrap();
    assert_eq!(serde_json::to_value(computer.catalog()).unwrap(), expected);
    assert_eq!(computer.tools().count(), 3);
}

#[tokio::test]
async fn rejects_repeated_cursors_and_duplicate_tool_names() {
    for (page, expected) in [
        (
            json!({"tools":[], "nextCursor":"opaque-next-page"}),
            "repeated tools/list cursor",
        ),
        (
            json!({"tools":[catalog()[0].clone()]}),
            "non-empty and unique",
        ),
    ] {
        let error = match ComputerTools::connect(paginated_config(page)).await {
            Ok(_) => panic!("invalid catalog was registered"),
            Err(error) => error,
        };
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[tokio::test]
async fn slow_provider_results_outlive_small_numeric_provider_budgets() {
    let computer = ComputerTools::connect(config()).await.unwrap();
    for name in ["js", "js_reset", "future_tool"] {
        let arguments = json!({"code":"slow", "timeout_ms":1});
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            computer
                .tool(name)
                .unwrap()
                .execute(input(arguments.clone()), context(name)),
        )
        .await
        .expect("synthetic provider did not finish")
        .unwrap();
        assert!(started.elapsed() >= Duration::from_millis(300));
        assert_eq!(
            result.structured_result()["structuredContent"]["call"]["params"]["arguments"],
            arguments
        );
        // A successful late result leaves the original realm available.
        let continued = computer
            .js()
            .execute(input(json!({})), context(name))
            .await
            .unwrap();
        assert_eq!(
            continued.structured_result()["structuredContent"]["count"],
            2
        );
    }
}

#[tokio::test]
async fn provider_startup_does_not_consume_the_provider_argument_budget() {
    let mut delayed = config();
    delayed.args[1] = format!("sleep 0.3\n{}", delayed.args[1].to_str().unwrap()).into();
    let computer = ComputerTools::connect(delayed).await.unwrap();
    // This new conversation starts another provider after discovery completes.
    let started = std::time::Instant::now();
    let arguments = json!({"timeout_ms":1});
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        computer
            .js()
            .execute(input(arguments.clone()), context("startup")),
    )
    .await
    .expect("synthetic provider did not start")
    .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(300));
    assert_eq!(
        result.structured_result()["structuredContent"]["call"]["params"]["arguments"],
        arguments
    );
    assert_eq!(result.structured_result()["structuredContent"]["count"], 1);
}

#[tokio::test]
async fn queued_time_does_not_consume_the_provider_argument_budget() {
    let computer = ComputerTools::connect(config()).await.unwrap();
    let js = computer.js();
    js.execute(input(json!({})), context("queue"))
        .await
        .unwrap();
    let arguments = json!({"timeout_ms":1});
    let (active, queued) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(
            js.execute(input(json!({"code":"slow"})), context("queue")),
            js.execute(input(arguments.clone()), context("queue")),
        )
    })
    .await
    .expect("synthetic queue did not drain");
    assert_eq!(
        active.unwrap().structured_result()["structuredContent"]["count"],
        2
    );
    let queued = queued.unwrap();
    assert_eq!(queued.structured_result()["structuredContent"]["count"], 3);
    assert_eq!(
        queued.structured_result()["structuredContent"]["call"]["params"]["arguments"],
        arguments
    );
    let continued = js
        .execute(input(json!({})), context("queue"))
        .await
        .unwrap();
    assert_eq!(
        continued.structured_result()["structuredContent"]["count"],
        4
    );
}

// Files provide a handshake with the real subprocess while Tokio's clock is
// paused. Waiting by yielding keeps virtual time from auto-advancing before the
// process actually reaches the phase under test.
struct StartupFixture {
    directory: std::path::PathBuf,
}
impl StartupFixture {
    fn new() -> Self {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("nanocodex-startup-{}-{unique}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        Self { directory }
    }
    fn config(&self) -> ComputerConfig {
        let mut config = config();
        config.environment.insert(
            "NANOCODEX_FIXTURE_DIRECTORY".into(),
            self.directory.clone().into_os_string(),
        );
        let script = config.args[1]
            .to_str()
            .unwrap()
            .replacen(
                "IFS= read -r initialize",
                r#"IFS= read -r initialize
if [ -e "$NANOCODEX_FIXTURE_DIRECTORY/stall" ]; then
    printf initialize > "$NANOCODEX_FIXTURE_DIRECTORY/phase"
    while [ ! -e "$NANOCODEX_FIXTURE_DIRECTORY/release" ]; do sleep 0.01; done
fi"#,
                1,
            )
            .replace(
                "IFS= read -r list\n",
                r#"IFS= read -r list
if [ -e "$NANOCODEX_FIXTURE_DIRECTORY/stall" ]; then
    printf catalog > "$NANOCODEX_FIXTURE_DIRECTORY/phase"
    IFS= read -r never
    exit 0
fi
"#,
            );
        config.args[1] = script.into();
        config
    }
    fn stall(&self) {
        std::fs::write(self.directory.join("stall"), b"").unwrap();
    }
    async fn wait_for_phase(&self, expected: &str) {
        let started = std::time::Instant::now();
        loop {
            if std::fs::read_to_string(self.directory.join("phase"))
                .ok()
                .as_deref()
                == Some(expected)
            {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "fixture did not reach {expected}"
            );
            tokio::task::yield_now().await;
        }
    }
}
impl Drop for StartupFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[tokio::test]
async fn trusted_startup_deadline_is_cumulative_across_initialize_and_catalog() {
    let fixture = StartupFixture::new();
    fixture.stall();
    let config = fixture.config();
    tokio::time::pause();
    let task = tokio::spawn(async move { ComputerTools::connect(config).await });
    fixture.wait_for_phase("initialize").await;
    tokio::time::advance(Duration::from_secs(70)).await;
    assert!(!task.is_finished());
    std::fs::write(fixture.directory.join("release"), b"").unwrap();
    fixture.wait_for_phase("catalog").await;
    tokio::time::advance(Duration::from_secs(49)).await;
    assert!(!task.is_finished());
    tokio::time::advance(Duration::from_secs(1)).await;
    let error = match task.await.unwrap() {
        Ok(_) => panic!("blocked catalog should hit the cumulative startup deadline"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("startup timed out after 120 seconds"),
        "{error}"
    );
}

#[tokio::test]
async fn conversation_startup_deadline_ignores_provider_budget_and_requires_reset() {
    let fixture = StartupFixture::new();
    let computer = ComputerTools::connect(fixture.config()).await.unwrap();
    fixture.stall();
    tokio::time::pause();
    let js = computer.js();
    let task = tokio::spawn(async move {
        js.execute(
            input(json!({"timeout_ms":900000})),
            context("startup-expiry"),
        )
        .await
    });
    fixture.wait_for_phase("initialize").await;
    tokio::time::advance(Duration::from_secs(119)).await;
    assert!(!task.is_finished());
    tokio::time::advance(Duration::from_secs(1)).await;
    let error = task.await.unwrap().err().unwrap().to_string();
    assert!(
        error.contains("startup timed out after 120 seconds"),
        "{error}"
    );
    let interrupted = computer
        .js()
        .execute(input(json!({})), context("startup-expiry"))
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(interrupted.contains("cua_repl.js_reset"), "{interrupted}");
    assert!(
        interrupted.contains("effects are uncertain"),
        "{interrupted}"
    );
    // Explicit reset can start a new transport once startup is responsive.
    tokio::time::resume();
    std::fs::remove_file(fixture.directory.join("stall")).unwrap();
    computer
        .reset()
        .execute(input(json!({})), context("startup-expiry"))
        .await
        .unwrap();
    let result = computer
        .js()
        .execute(input(json!({})), context("startup-expiry"))
        .await
        .unwrap();
    assert_eq!(result.structured_result()["structuredContent"]["count"], 1);
}
