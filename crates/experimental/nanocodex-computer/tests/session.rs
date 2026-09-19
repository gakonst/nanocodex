use nanocodex_computer::{ComputerConfig, ComputerRequest, ComputerTools};
use nanocodex_oai_api::tools::{Tool, ToolContext, ToolInput, ToolOutputBody, ToolOutputContent};
use serde_json::json;
use std::{path::PathBuf, time::Duration};

fn fixture() -> ComputerTools {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("runtime/target/debug/nanocodex-computer");
    assert!(
        path.is_file(),
        "Run pnpm test:computer to build the companion first"
    );
    let mut config = ComputerConfig::new(path);
    config.args.push("--fixture".into());
    ComputerTools::local(config)
}
fn context(session: &str) -> ToolContext<'_> {
    ToolContext::new("gpt-6-astra", session, "fixture-call", &[], 16000)
}
fn input(code: &str) -> ToolInput {
    ToolInput::Function(serde_json::value::to_raw_value(&json!({"code":code})).unwrap())
}

#[test]
fn validates_code_and_timeout_at_the_transport_boundary() {
    for timeout_ms in [30_000, 120_001, 300_000, 2_147_483_648] {
        assert!(
            ComputerRequest {
                code: "1".into(),
                title: None,
                timeout_ms
            }
            .validate()
            .is_ok()
        );
    }
    let defaults: ComputerRequest =
        serde_json::from_value(json!({"code":"1","title":null,"timeout_ms":null})).unwrap();
    assert_eq!(defaults.timeout_ms, 9_007_199_254_740_991);
    for timeout_ms in [0, 9_007_199_254_740_992, u64::MAX] {
        assert!(
            ComputerRequest {
                code: "1".into(),
                title: None,
                timeout_ms
            }
            .validate()
            .is_err()
        );
    }
    assert!(
        ComputerRequest {
            code: "a".repeat(1024 * 1024 + 1),
            title: None,
            timeout_ms: 1
        }
        .validate()
        .is_ok()
    );
}

#[tokio::test]
#[ignore = "requires built CUA companion; pnpm test:computer runs this"]
async fn long_timeout_and_current_call_metadata_cross_the_rust_adapter() {
    let computer = fixture();
    for call in ["first", "next"] {
        let args = json!({"code":"nodeRepl.write(JSON.stringify(nodeRepl.requestMeta));","timeout_ms":300_000,"title":null});
        let result = computer
            .js()
            .execute(
                ToolInput::Function(serde_json::value::to_raw_value(&args).unwrap()),
                ToolContext::new("owned-model", "metadata-thread", call, &[], 16000),
            )
            .await
            .unwrap();
        assert!(result.success);
        let text = result.structured_result()["content"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["text"]
            .as_str()
            .unwrap()
            .to_owned();
        let metadata: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            metadata["x-codex-turn-metadata"],
            json!({"thread_id":"metadata-thread","call_id":call,"model":"owned-model"})
        );
    }
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
#[ignore = "requires built CUA companion; pnpm test:computer runs this"]
async fn conversation_state_reset_and_screenshots_cross_the_real_process_boundary() {
    let computer = fixture();
    let js = computer.js();
    assert_eq!(
        serde_json::to_value(js.definition()).unwrap()["name"],
        "mcp__cua_repl__js"
    );
    assert_eq!(
        serde_json::to_value(computer.reset().definition()).unwrap()["name"],
        "mcp__cua_repl__js_reset"
    );
    let first = js
        .execute(
            input("let app = await cua.getApp('fixture://native');"),
            context("one"),
        )
        .await
        .unwrap();
    assert!(first.success);
    let metadata: serde_json::Value =
        serde_json::from_str(first.metadata.as_ref().unwrap().get()).unwrap();
    assert!(metadata["codex/nodeReplExecutionDurationMs"].is_number());
    assert_eq!(metadata, first.structured_result()["_meta"]);
    let changed = js.execute(input("await app.click(2); await app.getAXState(); await nodeRepl.emitImage(await app.getScreenshot({emit:false}));"), context("one")).await.unwrap();
    assert!(changed.success, "{}", changed.structured_result());
    let ToolOutputBody::Content(content) = changed.output else {
        panic!("Expected image content")
    };
    assert!(content.iter().any(|item| matches!(
        item,
        ToolOutputContent::InputImage {
            detail: nanocodex_oai_api::ImageDetail::Original,
            ..
        }
    )));
    let other = js
        .execute(input("nodeRepl.write(typeof app);"), context("two"))
        .await
        .unwrap();
    assert!(other.structured_result().to_string().contains("undefined"));
    computer
        .reset()
        .execute(
            ToolInput::Function(serde_json::value::to_raw_value(&json!({})).unwrap()),
            context("one"),
        )
        .await
        .unwrap();
    let reset = js
        .execute(input("nodeRepl.write(typeof app);"), context("one"))
        .await
        .unwrap();
    assert!(reset.structured_result().to_string().contains("undefined"));
}

#[tokio::test]
#[ignore = "requires built CUA companion; pnpm test:computer runs this"]
async fn independent_conversations_execute_in_parallel() {
    let computer = fixture();
    let js = computer.js();
    let left = js.clone();
    let right = js.clone();
    left.execute(input("nodeRepl.write('warm');"), context("parallel-left"))
        .await
        .unwrap();
    right
        .execute(input("nodeRepl.write('warm');"), context("parallel-right"))
        .await
        .unwrap();
    let started = std::time::Instant::now();
    let (left, right) = tokio::join!(
        left.execute(
            input("await new Promise(resolve=>setTimeout(resolve,1000));nodeRepl.write('left');"),
            context("parallel-left"),
        ),
        right.execute(
            input("await new Promise(resolve=>setTimeout(resolve,1000));nodeRepl.write('right');"),
            context("parallel-right"),
        ),
    );
    assert!(left.unwrap().success);
    assert!(right.unwrap().success);
    assert!(
        started.elapsed() < Duration::from_millis(1750),
        "independent CUA conversations were serialized"
    );
}

#[tokio::test]
#[ignore = "requires built CUA companion; pnpm test:computer runs this"]
async fn cancellation_stops_the_process_and_requires_an_explicit_reset() {
    let computer = fixture();
    let js = computer.js();
    js.execute(input("nodeRepl.write('ready');"), context("cancelled"))
        .await
        .unwrap();
    let active = js.clone();
    let task = tokio::spawn(async move {
        active
            .execute(input("while (true) {}"), context("cancelled"))
            .await
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    task.abort();
    let _ = task.await;
    assert!(
        js.execute(input("nodeRepl.write(1);"), context("cancelled"))
            .await
            .is_err()
    );
    computer
        .reset()
        .execute(
            ToolInput::Function(serde_json::value::to_raw_value(&json!({})).unwrap()),
            context("cancelled"),
        )
        .await
        .unwrap();
    assert!(
        js.execute(input("nodeRepl.write(2);"), context("cancelled"))
            .await
            .unwrap()
            .success
    );
}

#[tokio::test]
async fn background_mode_cannot_be_retargeted_to_a_private_desktop() {
    let mut config = ComputerConfig::new("/does/not/exist");
    config.desktop_runtime = Some("/does/not/exist".into());
    config
        .environment
        .insert("NANOCODEX_COMPUTER_BACKGROUND".into(), "hyprland".into());
    let computer = ComputerTools::local(config);
    let error = computer
        .js()
        .execute(
            input("await cua.getState()"),
            context("conflicting-desktops"),
        )
        .await
        .err()
        .expect("conflicting routing must fail");
    assert!(
        error
            .to_string()
            .contains("Background CUA cannot be combined")
    );
}

#[tokio::test]
#[ignore = "requires built CUA companion; pnpm test:computer runs this"]
async fn discovers_an_external_mcp_command_before_publishing_its_tools() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("runtime/target/debug/nanocodex-computer");
    let mut config = ComputerConfig::mcp(path);
    // These are the provider's exact args. Appending companion flags again
    // would cause the process to reject its command line.
    config.args = vec![
        "--fixture".into(),
        "--allow-native-control".into(),
        "serve".into(),
    ];
    let computer = ComputerTools::connect(config).await.unwrap();
    let definition = serde_json::to_value(computer.js().definition()).unwrap();
    assert!(definition.to_string().contains("mcp__cua_repl__js"));
    assert!(definition.to_string().contains("Control native apps"));
    assert!(
        computer
            .js()
            .execute(input("nodeRepl.write('discovered');"), context("discovery"))
            .await
            .unwrap()
            .success
    );
}

#[tokio::test]
#[ignore = "requires NANOCODEX_TEST_EXTERNAL_COMPUTER pointing to an installed external MCP launcher"]
async fn installed_external_provider_discovery_preserves_catalog_and_hides_lifecycle_hook() {
    let Some(executable) = std::env::var_os("NANOCODEX_TEST_EXTERNAL_COMPUTER") else {
        // The bundled runtime's --include-ignored suite needs no external install.
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
