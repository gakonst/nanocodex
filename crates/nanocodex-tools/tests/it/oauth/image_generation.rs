use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use eyre::Result;
use nanocodex_tools::{
    ToolContext, ToolInput, Tools,
    contract::DEFAULT_TOOL_OUTPUT_TOKENS,
    runtime::{ImageGenerationConfig, ToolRuntime},
};
use serde_json::{json, value::to_raw_value};
use tokio::{net::TcpListener, task::JoinHandle};

use crate::support::{
    auth::RotatingChatGptAuth,
    http::{CapturedRequest, read_request, write_json, write_unauthorized},
};

const TINY_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240, 31, 0,
    5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

#[tokio::test]
async fn chatgpt_auth_recovers_across_generation_and_edit_routes() -> Result<()> {
    let _runtime_test = crate::TOOL_RUNTIME_TEST_LOCK.lock().await;
    let workspace = TestWorkspace::new("image-oauth")?;
    let source_image = workspace.path().join("source.png");
    tokio::fs::write(&source_image, TINY_PNG).await?;
    let (api_base_url, server) = spawn_image_server().await?;
    let (auth, auth_source) = RotatingChatGptAuth::shared();
    let tools = Tools::builder()
        .without_defaults()
        .image_generation(true)
        .build()?;
    let runtime = ToolRuntime::new_with_tools(
        workspace.path(),
        None,
        Some(ImageGenerationConfig {
            api_base_url,
            auth,
            save_root: workspace.path().to_path_buf(),
        }),
        &tools,
    );

    let generated = runtime
        .execute_tool(
            "image_gen__imagegen",
            ToolInput::Function(to_raw_value(&json!({
                "prompt": "paint a blue whale",
            }))?),
            context("generate"),
        )
        .await;
    assert!(generated.success);

    let edited = runtime
        .execute_tool(
            "image_gen__imagegen",
            ToolInput::Function(to_raw_value(&json!({
                "prompt": "add a red hat",
                "referenced_image_paths": [source_image],
            }))?),
            context("edit"),
        )
        .await;
    assert!(edited.success);

    let requests = server.await??;
    assert_eq!(
        requests
            .iter()
            .map(|request| request.path.as_str())
            .collect::<Vec<_>>(),
        [
            "/v1/images/generations",
            "/v1/images/generations",
            "/v1/images/edits",
        ]
    );
    assert_oauth_headers(&requests[0].headers, "oauth-token-0");
    assert_oauth_headers(&requests[1].headers, "oauth-token-1");
    assert_oauth_headers(&requests[2].headers, "oauth-token-1");
    assert_eq!(auth_source.recoveries(), 1);
    Ok(())
}

const fn context(call_id: &str) -> ToolContext<'_> {
    ToolContext::new(
        "gpt-5.6-sol",
        "oauth-session",
        call_id,
        &[],
        DEFAULT_TOOL_OUTPUT_TOKENS,
    )
}

async fn spawn_image_server() -> Result<(String, JoinHandle<Result<Vec<CapturedRequest>>>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let api_base_url = format!("http://{}/v1", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let mut requests = Vec::with_capacity(3);
        for index in 0..3 {
            let (mut stream, _) = listener.accept().await?;
            requests.push(read_request(&mut stream).await?);
            if index == 0 {
                write_unauthorized(&mut stream).await?;
            } else {
                write_json(
                    &mut stream,
                    &json!({
                        "created": 1,
                        "data": [{"b64_json": BASE64_STANDARD.encode(TINY_PNG)}],
                        "background": "opaque",
                        "quality": "high",
                        "size": "1024x1024"
                    }),
                )
                .await?;
            }
        }
        Ok(requests)
    });
    Ok((api_base_url, server))
}

fn assert_oauth_headers(headers: &str, token: &str) {
    let headers = headers.to_ascii_lowercase();
    assert!(headers.contains(&format!("authorization: bearer {token}")));
    assert!(headers.contains("chatgpt-account-id: account-test"));
    assert!(headers.contains("x-openai-fedramp: true"));
}

struct TestWorkspace(PathBuf);

impl TestWorkspace {
    fn new(label: &str) -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "nanocodex-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
