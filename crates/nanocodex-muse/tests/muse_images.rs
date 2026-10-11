#![cfg(feature = "openai")]
#![allow(missing_docs)]

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use eyre::{Result, eyre};
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use nanocodex_muse::{
    ForkRequest, Muse, Nanocodex, Tools,
    input::{ImageDetail, Prompt, UserInput},
    tools::ToolExposure,
};
use nanocodex_oai_api::auth::{
    OpenAiAuth, OpenAiAuthError, OpenAiAuthFuture, OpenAiAuthMode, OpenAiAuthSnapshot,
    OpenAiAuthSource,
};
use serde_json::{Value, json};
use std::{
    io::Cursor,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

fn image(format: ImageFormat) -> Vec<u8> {
    let pixels = RgbImage::from_fn(256, 256, |x, _| {
        if x < 128 {
            Rgb([255, 0, 0])
        } else {
            Rgb([0, 0, 255])
        }
    });
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(pixels)
        .write_to(&mut bytes, format)
        .unwrap();
    bytes.into_inner()
}
fn data_url(format: &str, bytes: &[u8]) -> String {
    format!("data:image/{format};base64,{}", BASE64.encode(bytes))
}
fn image_request(prompt: &str, references: Vec<Value>) -> Value {
    let mut content = vec![json!({"type":"input_text","text":prompt})];
    for reference in references {
        let part = if let Some(url) = reference.get("image_url") {
            json!({"type":"input_image","image_url":url})
        } else {
            json!({"type":"input_image","file_id":reference["file_id"]})
        };
        content.push(part);
    }
    json!({"model":"muse-image-1.0","store":false,"input":[{"role":"user","content":content}],"tools":[{"type":"image_generation","output_format":"png","size":"auto"}]})
}
fn image_response(bytes: &[u8], format: &str) -> Value {
    json!({"id":"image-response","status":"completed","output":[{"type":"reasoning","summary":[]},{"type":"message","role":"assistant","content":[]},{"type":"image_generation_call","id":"ig-fixture","status":"completed","result":BASE64.encode(bytes),"output_format":format}]})
}
async fn request(listener: &TcpListener) -> Result<(TcpStream, String, String, Value)> {
    let (mut stream, _) = timeout(Duration::from_secs(15), listener.accept()).await??;
    let mut bytes = Vec::new();
    let end = loop {
        if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
            break end + 4;
        }
        if stream.read_buf(&mut bytes).await? == 0 {
            return Err(eyre!("request ended before headers"));
        }
    };
    let headers = String::from_utf8(bytes[..end].to_vec())?;
    let path = headers
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_owned();
    let length: usize = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|s| s.trim().to_owned())
        })
        .unwrap()
        .parse()?;
    while bytes.len() < end + length {
        if stream.read_buf(&mut bytes).await? == 0 {
            return Err(eyre!("request ended before body"));
        }
    }
    let body = serde_json::from_slice(&bytes[end..end + length])?;
    Ok((stream, path, headers, body))
}
async fn reply(mut stream: TcpStream, status: u16, content_type: &str, body: &str) -> Result<()> {
    stream.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}
fn answer(text: &str) -> Value {
    json!({"type":"message", "role":"assistant", "content":[{"type":"output_text","text":text}]})
}
async fn response(stream: TcpStream, id: &str, output: Vec<Value>, tokens: u64) -> Result<()> {
    let event = json!({"type":"response.completed","response":{"id":id,"status":"completed","output":output,"usage":{"input_tokens":tokens,"output_tokens":2,"total_tokens":tokens+2}}});
    reply(
        stream,
        200,
        "text/event-stream",
        &format!("data: {event}\n\ndata: [DONE]\n\n"),
    )
    .await
}
async fn turn(agent: &Nanocodex, prompt: impl Into<Prompt>) -> Result<String> {
    let prompt = prompt.into();
    let result = timeout(Duration::from_secs(20), async {
        agent.prompt(prompt).await?.result().await
    })
    .await??;
    Ok(result.final_message().to_owned())
}
fn tools(generation: bool, exposure: ToolExposure) -> Result<Tools> {
    Ok(Tools::builder()
        .without_defaults()
        .image_generation(generation)
        .exposure(exposure)
        .build()?)
}
fn images(body: &Value) -> Vec<&Value> {
    body["input"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|item| item["content"].as_array().into_iter().flatten())
        .filter(|part| part["type"] == "input_image")
        .collect()
}
fn receipt<'a>(body: &'a Value, call_id: &str) -> &'a Value {
    let input = body["input"].as_array().unwrap();
    let call = input
        .iter()
        .position(|item| item["type"] == "function_call" && item["call_id"] == call_id)
        .expect("stateless continuation must replay the matching tool call");
    let output = input
        .iter()
        .position(|item| item["type"] == "function_call_output" && item["call_id"] == call_id)
        .expect("tool receipt must be present");
    assert!(call < output, "the tool call must precede its receipt");
    assert!(
        body["tools"]
            .as_array()
            .is_some_and(|tools| !tools.is_empty())
    );
    &body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output" && item["call_id"] == call_id)
        .unwrap()["output"]
}
fn image_part(parts: &Value) -> &Value {
    parts
        .as_array()
        .expect("tool output must be typed content, not a JSON string")
        .iter()
        .find(|part| part["type"] == "input_image")
        .unwrap()
}

struct RotatingImageAuth(AtomicUsize);
impl OpenAiAuthSource for RotatingImageAuth {
    fn validate(&self) -> std::result::Result<(), OpenAiAuthError> {
        Ok(())
    }
    fn snapshot(
        &self,
    ) -> OpenAiAuthFuture<'_, std::result::Result<OpenAiAuthSnapshot, OpenAiAuthError>> {
        Box::pin(async {
            let revision = self.0.load(Ordering::SeqCst);
            Ok(OpenAiAuthSnapshot::new(
                OpenAiAuthMode::ApiKey,
                if revision == 0 {
                    "old-synthetic-key"
                } else {
                    "new-synthetic-key"
                },
                None::<String>,
                false,
                revision as u64,
            ))
        })
    }
    fn recover_unauthorized(
        &self,
        _rejected: &OpenAiAuthSnapshot,
    ) -> OpenAiAuthFuture<'_, std::result::Result<(), OpenAiAuthError>> {
        Box::pin(async {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}

#[tokio::test]
async fn muse_image_unauthorized_recovers_once_and_bounds_the_retry() -> Result<()> {
    for rejected_twice in [false, true] {
        let workspace = tempfile::tempdir()?;
        let png = image(ImageFormat::Png);
        let source = Arc::new(RotatingImageAuth(AtomicUsize::new(0)));
        let auth = OpenAiAuth::managed_api_key(source.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}/v1", listener.local_addr()?);
        let server = tokio::spawn(async move {
            let (stream, _, _, _) = request(&listener).await?;
            response(stream, "generation", vec![json!({"type":"function_call","call_id":"auth-gen","name":"exec","arguments":json!({"input":"try { const result = await tools.image_gen__imagegen({prompt:'paint a square'}); generatedImage(result); } catch (error) { text(error); }"}).to_string()})], 12).await?;
            let mut previous = None;
            for key in ["old-synthetic-key", "new-synthetic-key"] {
                let (stream, path, headers, body) = request(&listener).await?;
                assert_eq!(path, "/v1/responses");
                assert!(
                    headers
                        .to_lowercase()
                        .contains(&format!("authorization: bearer {key}"))
                );
                if let Some(previous) = &previous {
                    assert_eq!(&body, previous);
                }
                previous = Some(body);
                if key == "old-synthetic-key" || rejected_twice {
                    reply(
                        stream,
                        401,
                        "application/json",
                        r#"{"error":{"message":"Unauthorized"}}"#,
                    )
                    .await?;
                } else {
                    reply(
                        stream,
                        200,
                        "application/json",
                        &image_response(&png, "png").to_string(),
                    )
                    .await?;
                }
            }
            let (stream, path, _, body) = request(&listener).await?;
            assert_eq!(path, "/v1/responses");
            let output = receipt(&body, "auth-gen");
            if rejected_twice {
                assert!(output.to_string().contains("401"), "{output}");
            } else {
                assert_eq!(image_part(output)["image_url"], data_url("png", &png));
            }
            response(stream, "done", vec![answer("Finished.")], 12).await?;
            Result::<()>::Ok(())
        });
        let (agent, _) = Nanocodex::builder(Muse::builder(auth).api_base_url(endpoint).build()?)
            .workspace(workspace.path())
            .tools(tools(true, ToolExposure::CodeModeOnly)?)
            .build()?;
        let result = turn(&agent, "Generate an image.").await;
        agent.shutdown().await?;
        server.await??;
        assert_eq!(result?, "Finished.");
        assert_eq!(source.0.load(Ordering::SeqCst), 1);
        assert_eq!(
            workspace.path().join("generated_images").exists(),
            !rejected_twice
        );
    }
    Ok(())
}

#[tokio::test]
async fn muse_edits_accept_local_remote_and_uploaded_references() -> Result<()> {
    for source in ["local", "remote", "uploaded"] {
        let workspace = tempfile::tempdir()?;
        let png = image(ImageFormat::Png);
        let path = workspace.path().join("reference.png");
        std::fs::write(&path, &png)?;
        let (prompt, args, reference) = match source {
            "local" => (
                Prompt::from("Edit the reference image."),
                json!({"prompt":"add a circle","referenced_image_paths":[path]}),
                json!({"image_url":data_url("png", &png)}),
            ),
            "remote" => (
                Prompt::content(vec![UserInput::Image {
                    image_url: "https://example.com/reference.png".into(),
                    detail: None,
                }]),
                json!({"prompt":"add a circle","num_last_images_to_include":1}),
                json!({"image_url":"https://example.com/reference.png"}),
            ),
            _ => (
                Prompt::content(vec![UserInput::ImageFile {
                    file_id: "file-reference".into(),
                    detail: None,
                }]),
                json!({"prompt":"add a circle","num_last_images_to_include":1}),
                json!({"file_id":"file-reference"}),
            ),
        };
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}/v1", listener.local_addr()?);
        let server = tokio::spawn(async move {
            let (stream, _, _, _) = request(&listener).await?;
            response(stream, "edit", vec![json!({"type":"function_call","call_id":"edit-ref","name":"exec","arguments":json!({"input":format!("try {{ const result = await tools.image_gen__imagegen({args}); generatedImage(result); }} catch (error) {{ text(error); }}")}).to_string()})], 12).await?;
            let (stream, path, _, body) = request(&listener).await?;
            assert_eq!(path, "/v1/responses");
            assert_eq!(body, image_request("add a circle", vec![reference]));
            reply(
                stream,
                200,
                "application/json",
                &image_response(&png, "png").to_string(),
            )
            .await?;
            let (stream, _, _, body) = request(&listener).await?;
            assert_eq!(
                image_part(receipt(&body, "edit-ref"))["image_url"],
                data_url("png", &png)
            );
            response(stream, "done", vec![answer("Edited.")], 12).await?;
            Result::<()>::Ok(())
        });
        let (agent, _) = Nanocodex::builder(
            Muse::builder("synthetic-key")
                .api_base_url(endpoint)
                .build()?,
        )
        .workspace(workspace.path())
        .tools(tools(true, ToolExposure::CodeModeOnly)?)
        .build()?;
        let result = turn(&agent, prompt).await;
        agent.shutdown().await?;
        server.await??;
        assert_eq!(result?, "Edited.");
    }
    Ok(())
}

#[tokio::test]
async fn muse_image_failures_return_text_without_creating_artifacts() -> Result<()> {
    for transparent in [false, true] {
        let workspace = tempfile::tempdir()?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}/v1", listener.local_addr()?);
        let server = tokio::spawn(async move {
            let (stream, _, _, _) = request(&listener).await?;
            let args = json!({"prompt":"paint a square","transparent_background":transparent});
            response(stream, "generation", vec![json!({"type":"function_call","call_id":"failed-gen","name":"exec","arguments":json!({"input":format!("try {{ const result = await tools.image_gen__imagegen({args}); generatedImage(result); }} catch (error) {{ text(error); }}")}).to_string()})], 12).await?;
            if !transparent {
                let (stream, path, _, _) = request(&listener).await?;
                assert_eq!(path, "/v1/responses");
                reply(
                    stream,
                    400,
                    "application/json",
                    r#"{"error":{"message":"Image request rejected"}}"#,
                )
                .await?;
            }
            let (stream, path, _, body) = request(&listener).await?;
            assert_eq!(path, "/v1/responses");
            let error = receipt(&body, "failed-gen").to_string();
            assert!(
                error.contains(if transparent { "opaque" } else { "400" }),
                "{error}"
            );
            response(stream, "done", vec![answer("Image unavailable.")], 12).await?;
            Result::<()>::Ok(())
        });
        let (agent, _) = Nanocodex::builder(
            Muse::builder("synthetic-key")
                .api_base_url(endpoint)
                .build()?,
        )
        .workspace(workspace.path())
        .tools(tools(true, ToolExposure::CodeModeOnly)?)
        .build()?;
        let result = turn(&agent, "Generate an image.").await;
        agent.shutdown().await?;
        server.await??;
        assert_eq!(result?, "Image unavailable.");
        assert!(!workspace.path().join("generated_images").exists());
    }
    Ok(())
}

#[tokio::test]
async fn muse_image_inputs_match_the_documented_responses_payload_and_survive_resume_and_fork()
-> Result<()> {
    let workspace = tempfile::tempdir()?;
    let png = image(ImageFormat::Png);
    let jpeg = image(ImageFormat::Jpeg);
    let png_path = workspace.path().join("colors.png");
    let jpeg_path = workspace.path().join("colors.jpeg");
    std::fs::write(&png_path, &png)?;
    std::fs::write(&jpeg_path, &jpeg)?;
    let inline = data_url("png", &png);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/v1", listener.local_addr()?);
    let expected_inline = inline.clone();
    let server = tokio::spawn(async move {
        for id in ["first", "resumed", "forked"] {
            let (stream, path, headers, body) = request(&listener).await?;
            assert_eq!(path, "/v1/responses");
            assert!(headers.to_lowercase().contains("x-api-version: 1.0.0"));
            assert_eq!(body["model"], "muse-spark-1.3");
            assert_eq!(body["store"], false);
            let parts = images(&body);
            assert_eq!(parts.len(), 5);
            assert_eq!(
                parts[0],
                &json!({"type":"input_image","image_url":data_url("png", &png),"detail":"original"})
            );
            assert_eq!(
                parts[1],
                &json!({"type":"input_image","image_url":data_url("jpeg", &jpeg),"detail":"high"})
            );
            assert_eq!(
                parts[2],
                &json!({"type":"input_image","image_url":expected_inline,"detail":"low"})
            );
            assert_eq!(
                parts[3],
                &json!({"type":"input_image","image_url":"https://example.com/reference.png","detail":"auto"})
            );
            assert_eq!(
                parts[4],
                &json!({"type":"input_image","file_id":"file-synthetic-image","detail":"original"})
            );
            response(stream, id, vec![answer("red left, blue right")], 12).await?;
        }
        Result::<()>::Ok(())
    });
    let provider = Muse::builder("synthetic-key")
        .api_base_url(&endpoint)
        .build()?;
    let (agent, _) = Nanocodex::builder(provider)
        .workspace(workspace.path())
        .tools(tools(false, ToolExposure::CodeModeOnly)?)
        .build()?;
    let prompt = Prompt::content([
        UserInput::Text {
            text: "Compare these images".into(),
        },
        UserInput::LocalImage {
            path: png_path,
            detail: Some(ImageDetail::Original),
        },
        UserInput::LocalImage {
            path: jpeg_path,
            detail: Some(ImageDetail::High),
        },
        UserInput::Image {
            image_url: inline,
            detail: Some(ImageDetail::Low),
        },
        UserInput::Image {
            image_url: "https://example.com/reference.png".into(),
            detail: Some(ImageDetail::Auto),
        },
        UserInput::ImageFile {
            file_id: "file-synthetic-image".into(),
            detail: Some(ImageDetail::Original),
        },
    ]);
    assert_eq!(turn(&agent, prompt).await?, "red left, blue right");
    let snapshot = serde_json::from_slice(&serde_json::to_vec(&agent.checkpoint().await?)?)?;
    agent.shutdown().await?;
    let provider = Muse::builder("synthetic-key")
        .api_base_url(endpoint)
        .build()?;
    let (resumed, _) = Nanocodex::builder(provider)
        .resume(snapshot)?
        .tools(tools(false, ToolExposure::CodeModeOnly)?)
        .build()?;
    assert_eq!(
        turn(&resumed, "Describe the images again").await?,
        "red left, blue right"
    );
    let (forked, _) = resumed
        .fork(ForkRequest::at(resumed.checkpoint().await?))
        .await?;
    assert_ne!(forked.session_id(), resumed.session_id());
    assert_eq!(
        turn(&forked, "Compare the original images in this fork").await?,
        "red left, blue right"
    );
    forked.shutdown().await?;
    resumed.shutdown().await?;
    server.await??;
    Ok(())
}

#[tokio::test]
async fn spark_generates_inspects_compacts_and_edits_muse_images() -> Result<()> {
    generation_journey().await
}
async fn generation_journey() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let png = image(ImageFormat::Png);
    let webp = image(ImageFormat::WebP);
    let expected_png = data_url("png", &png);
    let expected_webp = data_url("webp", &webp);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/v1", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, path, _, body) = request(&listener).await?;
        assert_eq!(path, "/v1/responses");
        assert_eq!(body["model"], "muse-spark-1.3-contributor");
        let name = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| {
                tool["name"]
                    .as_str()
                    .is_some_and(|name| name == "exec" || name.ends_with("__exec"))
            })
            .unwrap()["name"]
            .clone();
        let args = json!({"input":"const result = await tools.image_gen__imagegen({prompt: 'paint a red and blue square'}); generatedImage(result);"});
        response(stream, "call-generation", vec![json!({"type":"function_call","call_id":"gen-fixture","name":name,"arguments":args.to_string()})], 9_000).await?;
        let (stream, path, headers, body) = request(&listener).await?;
        assert_eq!(path, "/v1/responses");
        assert!(headers.to_lowercase().contains("x-api-version: 1.0.0"));
        assert_eq!(body, image_request("paint a red and blue square", vec![]));
        reply(
            stream,
            200,
            "application/json",
            &image_response(&png, "png").to_string(),
        )
        .await?;
        let (stream, path, _, body) = request(&listener).await?;
        assert_eq!(path, "/v1/responses");
        assert_eq!(body["tool_choice"], "none");
        assert!(body.to_string().contains("Summarize the conversation"));
        response(
            stream,
            "summary",
            vec![answer(
                "The user requested a red and blue square. Inspect the tool result, then edit it.",
            )],
            12,
        )
        .await?;
        let (stream, path, _, body) = request(&listener).await?;
        assert_eq!(path, "/v1/responses");
        let generated = receipt(&body, "gen-fixture");
        assert_eq!(image_part(generated)["image_url"], expected_png);
        assert!(body.to_string().contains("muse_context_summary"));
        let args = json!({"input":"const result = await tools.image_gen__imagegen({prompt: 'add a white circle', num_last_images_to_include: 1}); generatedImage(result);"});
        response(stream, "call-edit", vec![json!({"type":"function_call","call_id":"edit-fixture","name":name,"arguments":args.to_string()})], 12).await?;
        let (stream, path, _, body) = request(&listener).await?;
        assert_eq!(path, "/v1/responses");
        assert_eq!(
            body,
            image_request(
                "add a white circle",
                vec![json!({"image_url":expected_png})]
            )
        );
        reply(
            stream,
            200,
            "application/json",
            &image_response(&webp, "webp").to_string(),
        )
        .await?;
        let (stream, path, _, body) = request(&listener).await?;
        assert_eq!(path, "/v1/responses");
        assert_eq!(
            image_part(receipt(&body, "edit-fixture"))["image_url"],
            expected_webp
        );
        response(
            stream,
            "final",
            vec![answer("Generated and edited the image.")],
            12,
        )
        .await?;
        Result::<()>::Ok(())
    });
    let provider = Muse::builder("synthetic-key")
        .model(nanocodex_muse::MuseModel::Contributor)
        .api_base_url(endpoint)
        .context_window_tokens(40_000)
        .build()?;
    let (agent, _) = Nanocodex::builder(provider)
        .workspace(workspace.path())
        .tools(tools(true, ToolExposure::CodeModeOnly)?)
        .build()?;
    let result = turn(
        &agent,
        "Generate a red and blue square, inspect it, then add a white circle.",
    )
    .await;
    agent.shutdown().await?;
    server.await??;
    assert_eq!(result?, "Generated and edited the image.");
    let root = workspace.path().join("generated_images");
    let mut files = Vec::new();
    for session in std::fs::read_dir(root)? {
        for file in std::fs::read_dir(session?.path())? {
            files.push(file?.path());
        }
    }
    assert_eq!(files.len(), 2);
    assert!(
        files
            .iter()
            .any(|path| path.extension().is_some_and(|extension| extension == "png"))
    );
    assert!(files.iter().any(|path| {
        path.extension()
            .is_some_and(|extension| extension == "webp")
    }));
    for path in files {
        image::load_from_memory(&std::fs::read(path)?)?;
    }
    Ok(())
}

#[tokio::test]
async fn muse_preserves_mcp_screenshots_as_typed_image_tool_results() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let png = image(ImageFormat::Png);
    let encoded = BASE64.encode(&png);
    let fixture = workspace.path().join("screenshot-server.mjs");
    std::fs::write(
        &fixture,
        format!(
            r#"import readline from 'node:readline';
const lines = readline.createInterface({{input:process.stdin}});
for await (const line of lines) {{
 const request = JSON.parse(line); let result;
 if (request.method === 'initialize') result={{protocolVersion:request.params.protocolVersion,capabilities:{{tools:{{}}}},serverInfo:{{name:'screenshot',version:'1'}}}};
 else if (request.method === 'tools/list') result={{tools:[{{name:'screenshot',description:'Return a red and blue screenshot',inputSchema:{{type:'object',properties:{{}}}}}}]}};
 else if (request.method === 'tools/call') result={{content:[{{type:'text',text:'Screenshot:'}},{{type:'image',data:'{encoded}',mimeType:'image/png',_meta:{{'codex/imageDetail':'low'}}}}]}};
 if (request.id !== undefined && result) console.log(JSON.stringify({{jsonrpc:'2.0',id:request.id,result}}));
}}"#
        ),
    )?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/v1", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _, _, _) = request(&listener).await?;
        let search = json!({"input":"text(await tools.tool_search({query:'red blue screenshot',limit:1}));"});
        response(stream, "search", vec![json!({"type":"function_call","call_id":"search-fixture","name":"exec","arguments":search.to_string()})], 12).await?;
        let (stream, _, _, body) = request(&listener).await?;
        assert!(
            receipt(&body, "search-fixture")
                .to_string()
                .contains("mcp__screenshot__")
        );
        let capture = json!({"input":"const result = await tools.mcp__screenshot__screenshot({}); text(result.content[0].text); image(result.content[1]);"});
        response(stream, "capture", vec![json!({"type":"function_call","call_id":"screenshot-fixture","name":"exec","arguments":capture.to_string()})], 12).await?;
        let (stream, _, _, body) = request(&listener).await?;
        let parts = receipt(&body, "screenshot-fixture");
        assert!(
            parts
                .as_array()
                .unwrap()
                .iter()
                .any(|part| part == &json!({"type":"input_text","text":"Screenshot:"})),
            "{parts}"
        );
        assert_eq!(
            image_part(parts),
            &json!({"type":"input_image","image_url":data_url("png", &png)})
        );
        response(stream, "analysis", vec![answer("red left, blue right")], 12).await?;
        Result::<()>::Ok(())
    });
    let mcp = nanocodex_muse::tools::mcp::Mcp::builder()
        .server(
            "screenshot",
            nanocodex_muse::tools::mcp::McpServer::stdio("node").arg(fixture.to_string_lossy()),
        )
        .build()?;
    let tools = Tools::builder()
        .without_defaults()
        .provider(mcp)
        .exposure(ToolExposure::CodeModeOnly)
        .build()?;
    let provider = Muse::builder("synthetic-key")
        .api_base_url(endpoint)
        .build()?;
    let (agent, _) = Nanocodex::builder(provider)
        .workspace(workspace.path())
        .tools(tools)
        .build()?;
    let result = turn(&agent, "Take a screenshot and describe it").await;
    agent.shutdown().await?;
    server.await??;
    assert_eq!(result?, "red left, blue right");
    Ok(())
}
