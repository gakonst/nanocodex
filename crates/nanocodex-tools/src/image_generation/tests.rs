use std::path::PathBuf;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use eyre::{Result, eyre};
use nanocodex_core::ResponseItem;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

use super::{
    ImageGenerationConfig, ImageGenerationHandler, ImageRequest, ImagegenArgs, Tool, ToolContext,
    ToolOutputBody, ToolOutputContent, request_for_args,
};

const TINY_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240, 31, 0,
    5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

#[tokio::test]
async fn generation_uses_codex_images_request_and_persists_result() -> Result<()> {
    let workspace = temporary_workspace("image-generation")?;
    let (api_base_url, server) = spawn_image_server().await?;
    let handler = ImageGenerationHandler::new(ImageGenerationConfig {
        api_base_url,
        auth: nanocodex_core::OpenAiAuth::api_key("test-key"),
        save_root: workspace.clone(),
    });
    let history = Vec::new();

    let execution = handler
        .run(
            r#"{"prompt":"paint a blue whale"}"#,
            ToolContext {
                model: "gpt-5.6-sol",
                session_id: "session/one",
                call_id: "code:1",
                history: &history,
                output_token_budget: crate::DEFAULT_TOOL_OUTPUT_TOKENS,
            },
        )
        .await;

    assert!(execution.success);
    let encoded = BASE64_STANDARD.encode(TINY_PNG);
    let ToolOutputBody::Content(content) = &execution.output else {
        return Err(eyre!("image generation did not return content items"));
    };
    assert!(matches!(
        content.first(),
        Some(ToolOutputContent::InputImage { image_url, detail: crate::ImageDetail::High })
            if image_url == &format!("data:image/png;base64,{encoded}")
    ));
    assert!(matches!(
        content.get(1),
        Some(ToolOutputContent::InputText { text })
            if text.contains("Generated images are saved")
                && text.contains("already displayed to the user")
    ));
    assert_eq!(
        execution.value()["image_url"],
        format!("data:image/png;base64,{encoded}")
    );
    let saved_path = workspace
        .join("generated_images")
        .join("session_one")
        .join("code_1.png");
    assert_eq!(tokio::fs::read(saved_path).await?, TINY_PNG);

    let request = server.await??;
    assert_eq!(request.path, "/v1/images/generations");
    assert!(
        request
            .headers
            .to_ascii_lowercase()
            .contains("authorization: bearer test-key")
    );
    assert_eq!(
        request.body,
        json!({
            "prompt": "paint a blue whale",
            "background": "auto",
            "model": "gpt-image-2",
            "quality": "auto",
            "size": "auto"
        })
    );
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn edit_accepts_original_local_images_and_recent_conversation_images() -> Result<()> {
    let workspace = temporary_workspace("image-edit")?;
    let local_path = workspace.join("source.png");
    tokio::fs::write(&local_path, TINY_PNG).await?;
    let local_request = request_for_args(
        &ImagegenArgs {
            prompt: "add a red hat".to_owned(),
            referenced_image_paths: Some(vec![local_path]),
            num_last_images_to_include: None,
        },
        &[],
    )
    .await
    .map_err(|error| eyre!(error))?;
    let ImageRequest::Edit(local_request) = local_request else {
        return Err(eyre!("local reference should select image editing"));
    };
    assert_eq!(local_request.images.len(), 1);
    assert!(
        local_request.images[0]
            .image_url
            .starts_with("data:image/png;base64,")
    );

    let history: Vec<ResponseItem> = serde_json::from_value(json!([
        json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_image", "image_url": "data:image/png;base64,old"}]
        }),
        json!({"type": "custom_tool_call", "call_id": "call-image", "name": "exec", "input": ""}),
        json!({
            "type": "custom_tool_call_output",
            "call_id": "call-image",
            "output": [
                {"type": "input_text", "text": "status"},
                {"type": "input_image", "image_url": "data:image/png;base64,new"}
            ]
        }),
    ]))?;
    let recent_request = request_for_args(
        &ImagegenArgs {
            prompt: "combine these".to_owned(),
            referenced_image_paths: None,
            num_last_images_to_include: Some(2),
        },
        &history,
    )
    .await
    .map_err(|error| eyre!(error))?;
    let ImageRequest::Edit(recent_request) = recent_request else {
        return Err(eyre!("conversation references should select image editing"));
    };
    assert_eq!(
        recent_request
            .images
            .iter()
            .map(|image| image.image_url.as_str())
            .collect::<Vec<_>>(),
        vec!["data:image/png;base64,old", "data:image/png;base64,new"]
    );
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[test]
fn exposes_codex_imagegen_shape() {
    let handler = ImageGenerationHandler::new(ImageGenerationConfig {
        api_base_url: "http://127.0.0.1:1/v1".to_owned(),
        auth: nanocodex_core::OpenAiAuth::api_key("test-key"),
        save_root: PathBuf::from("/tmp/nanocodex-imagegen-test"),
    });
    let spec = serde_json::to_value(handler.definition()).unwrap();

    assert_eq!(spec["name"], "image_gen__imagegen");
    assert_eq!(spec["strict"], false);
    assert_eq!(
        spec.pointer("/parameters/properties/referenced_image_paths/maxItems"),
        Some(&json!(5))
    );
    assert!(
        spec["description"]
            .as_str()
            .is_some_and(|description| description.contains("generatedImage(result)"))
    );
    assert!(spec.get("output_schema").is_none());
}

struct CapturedRequest {
    path: String,
    headers: String,
    body: Value,
}

async fn spawn_image_server() -> Result<(String, JoinHandle<Result<CapturedRequest>>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let api_base_url = format!("http://{}/v1", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let request = read_http_request(&mut stream).await?;
        let response = serde_json::to_vec(&json!({
            "created": 1,
            "data": [{"b64_json": BASE64_STANDARD.encode(TINY_PNG)}],
            "background": "opaque",
            "quality": "high",
            "size": "1024x1024"
        }))?;
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    response.len()
                )
                .as_bytes(),
            )
            .await?;
        stream.write_all(&response).await?;
        Ok(request)
    });
    Ok((api_base_url, server))
}

async fn read_http_request(stream: &mut TcpStream) -> Result<CapturedRequest> {
    let mut bytes = Vec::new();
    let header_end = loop {
        let mut chunk = [0_u8; 1024];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(eyre!("HTTP request ended before its headers"));
        }
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = std::str::from_utf8(&bytes[..header_end])?.to_owned();
    let request_line = headers
        .lines()
        .next()
        .ok_or_else(|| eyre!("HTTP request omitted request line"))?;
    let path = request_line
        .split_ascii_whitespace()
        .nth(1)
        .ok_or_else(|| eyre!("HTTP request line omitted path"))?
        .to_owned();
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .ok_or_else(|| eyre!("HTTP request omitted content-length"))?;
    while bytes.len() - header_end < content_length {
        let mut chunk = [0_u8; 1024];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(eyre!("HTTP request body ended early"));
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    Ok(CapturedRequest {
        path,
        headers,
        body: serde_json::from_slice(&bytes[header_end..header_end + content_length])?,
    })
}

fn temporary_workspace(label: &str) -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "nanocodex-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}
