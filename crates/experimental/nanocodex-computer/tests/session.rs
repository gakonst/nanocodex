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
    for timeout_ms in [0, 120_001, u64::MAX] {
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
        .is_err()
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
