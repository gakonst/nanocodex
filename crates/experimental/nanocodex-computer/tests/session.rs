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
async fn cancellation_stops_only_its_process_and_requires_explicit_reset() {
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
    assert!(
        computer
            .js()
            .execute(input(json!({})), context("cancelled"))
            .await
            .is_err()
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
