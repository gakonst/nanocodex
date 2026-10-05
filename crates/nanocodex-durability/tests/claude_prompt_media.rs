//! Real Messages HTTP + SQLite image prompt journeys; synthetic media only.
#![cfg(all(feature = "claude", feature = "sqlite"))]
use axum::{Json, Router, routing::post};
use nanocodex_agent::{
    Nanocodex, PromptRequest,
    input::{Prompt, UserInput},
};
use nanocodex_claude::{Claude, ClaudeClient};
use nanocodex_durability::{DurableAgentExt, DurableSession, SqliteStore};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a9e8AAAAASUVORK5CYII=";
fn data() -> String {
    format!("data:image/png;base64,{PNG}")
}
fn png_bytes() -> Vec<u8> {
    // Decode through the public input data URL in tests without another dependency.
    vec![
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 4,
        0, 0, 0, 181, 28, 12, 2, 0, 0, 0, 11, 73, 68, 65, 84, 120, 218, 99, 252, 255, 31, 0, 3, 3,
        2, 0, 239, 154, 245, 239, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ]
}
fn sse() -> String {
    [json!({"type":"message_start","message":{"id":"synthetic","role":"assistant","model":"test","content":[],"usage":{"input_tokens":1,"output_tokens":0}}}),
     json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"image received"}}),
     json!({"type":"content_block_stop","index":0}),
     json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}),
     json!({"type":"message_stop"})].into_iter().map(|v| format!("data: {v}\n\n")).collect()
}
async fn server(
    block_first: bool,
) -> (
    ClaudeClient,
    Arc<Mutex<Vec<Value>>>,
    Arc<Notify>,
    Arc<Notify>,
    tokio::task::JoinHandle<()>,
) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (log, begin, end) = (requests.clone(), started.clone(), release.clone());
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let (log, begin, end) = (log.clone(), begin.clone(), end.clone());
            async move {
                let first = {
                    let mut log = log.lock().unwrap();
                    log.push(body.clone());
                    log.len() == 1
                };
                eprintln!("{}", json!({"observed_request":body}));
                begin.notify_one();
                if block_first && first {
                    end.notified().await;
                }
                ([("content-type", "text/event-stream")], sse())
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (
        ClaudeClient::new(reqwest::Client::new(), url, "synthetic"),
        requests,
        started,
        release,
        task,
    )
}

#[tokio::test]
async fn ordered_images_survive_changed_then_deleted_local_file_and_sqlite_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("pixel.png");
    let db = dir.path().join("media.sqlite");
    std::fs::write(&file, png_bytes()).unwrap();
    let prompt = || {
        PromptRequest::new(Prompt::content([
            UserInput::Text {
                text: "first".into(),
            },
            UserInput::Image {
                image_url: "https://example.com/image.png".into(),
                detail: None,
            },
            UserInput::Text {
                text: "between".into(),
            },
            UserInput::Image {
                image_url: data(),
                detail: None,
            },
            UserInput::LocalImage {
                path: file.clone(),
                detail: None,
            },
        ]))
        .request_id("image-operation")
    };
    let (client, requests, _, _, task) = server(false).await;
    for pass in 0..3 {
        let session = DurableSession::open(SqliteStore::open(&db).unwrap(), "image-session")
            .await
            .unwrap();
        let (agent, events) = Nanocodex::builder(Claude::new(client.clone(), "test"))
            .durability(session)
            .await
            .unwrap()
            .build()
            .unwrap();
        let answer = agent
            .prompt(prompt())
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
        assert_eq!(answer.final_message(), "image received");
        agent.shutdown().await.unwrap();
        drop((agent, events));
        if pass == 0 {
            std::fs::write(&file, b"changed invalid image").unwrap();
        }
        if pass == 1 {
            std::fs::remove_file(&file).unwrap();
        }
    }
    let requests = requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        1,
        "replay must not send another HTTP request"
    );
    let blocks = requests[0]["messages"][0]["content"].as_array().unwrap();
    assert_eq!(
        blocks
            .iter()
            .map(|v| v["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["text", "image", "text", "image", "image"]
    );
    assert_eq!(blocks[0]["text"], "first");
    assert_eq!(blocks[2]["text"], "between");
    assert_eq!(blocks[1]["source"]["url"], "https://example.com/image.png");
    assert_eq!(blocks[3]["source"]["data"], PNG);
    assert_eq!(blocks[4]["source"], blocks[3]["source"]);
    eprintln!(
        "PASS ordered text/url/data/local blocks; changed+deleted file terminal replay; HTTP requests=1"
    );
    task.abort();
}

#[tokio::test]
async fn accepted_queued_local_image_is_frozen_before_file_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("pixel.png");
    std::fs::write(&file, png_bytes()).unwrap();
    let (client, requests, started, release, task) = server(true).await;
    let (agent, events) = Nanocodex::builder(Claude::new(client, "test"))
        .build()
        .unwrap();
    let first = agent.prompt("block first turn").await.unwrap();
    started.notified().await;
    let second = agent
        .prompt(Prompt::content([UserInput::LocalImage {
            path: file.clone(),
            detail: None,
        }]))
        .await
        .unwrap();
    std::fs::remove_file(&file).unwrap();
    release.notify_one();
    first.result().await.unwrap();
    second.result().await.unwrap();
    agent.shutdown().await.unwrap();
    drop((agent, events));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1]["messages"].as_array().unwrap().last().unwrap()["content"][0]["source"]["data"],
        PNG
    );
    eprintln!("PASS queued accepted local image retained original bytes after deletion");
    task.abort();
}

#[tokio::test]
async fn invalid_or_provider_specific_media_fails_before_http() {
    let dir = tempfile::tempdir().unwrap();
    let invalid_file = dir.path().join("fake.png");
    std::fs::write(&invalid_file, b"not an image").unwrap();
    let oversized = dir.path().join("large.png");
    std::fs::File::create(&oversized)
        .unwrap()
        .set_len(5 * 1024 * 1024 + 1)
        .unwrap();
    let (client, requests, _, _, task) = server(false).await;
    let (agent, events) = Nanocodex::builder(Claude::new(client, "test"))
        .build()
        .unwrap();
    let mut invalid = vec![
        UserInput::ImageFile {
            file_id: "file-synthetic".into(),
            detail: None,
        },
        UserInput::Audio {
            audio_url: "https://example.com/audio.wav".into(),
        },
        UserInput::LocalAudio {
            path: invalid_file.clone(),
        },
        UserInput::LocalImage {
            path: dir.path().into(),
            detail: None,
        },
        UserInput::LocalImage {
            path: invalid_file,
            detail: None,
        },
        UserInput::LocalImage {
            path: oversized,
            detail: None,
        },
    ];
    for value in [
        "http://example.com/a.png",
        "https://user:secret@example.com/a.png",
        "data:image/png;base64,garbage",
        "data:image/svg+xml;base64,PHN2Zz4=",
        "data:image/jpeg;base64,iVBORw0KGgo=",
    ] {
        invalid.push(UserInput::Image {
            image_url: value.into(),
            detail: None,
        });
    }
    for item in invalid {
        let result = agent.prompt(Prompt::content([item])).await;
        assert!(result.is_err(), "invalid media must fail before acceptance");
    }
    let many = (0..21).map(|_| UserInput::Image {
        image_url: data(),
        detail: None,
    });
    assert!(agent.prompt(Prompt::content(many)).await.is_err());
    assert!(requests.lock().unwrap().is_empty());
    agent.shutdown().await.unwrap();
    drop((agent, events));
    eprintln!(
        "PASS invalid MIME/bytes/base64/HTTPS/credentials, nonregular+oversized local files, opaque file IDs, audio, image-count bound: 0 HTTP requests"
    );
    task.abort();
}
