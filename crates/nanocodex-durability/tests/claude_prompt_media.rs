//! Real Messages HTTP + SQLite image prompt journeys; synthetic media only.
#![cfg(all(feature = "claude", feature = "sqlite"))]
use axum::{Json, Router, routing::post};
use nanocodex_agent::{
    Nanocodex, PromptRequest,
    input::{Prompt, UserInput},
};
use nanocodex_claude::{Claude, ClaudeClient};
use nanocodex_durability::{
    DurableAgentExt, DurableSession, OwnedState, OwnerId, OwnerToken, SqliteStore, StateStore,
    StoreError, StoreFuture, StoreRecord,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII=";
fn data() -> String {
    format!("data:image/png;base64,{PNG}")
}
fn png_bytes() -> Vec<u8> {
    // Decode through the public input data URL in tests without another dependency.
    vec![
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 4,
        0, 0, 0, 181, 28, 12, 2, 0, 0, 0, 11, 73, 68, 65, 84, 120, 218, 99, 252, 255, 31, 0, 3, 3,
        2, 0, 239, 162, 167, 91, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
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
async fn ordered_media_survives_changed_then_deleted_local_file_and_sqlite_reopen() {
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
            UserInput::Image {
                image_url: "data:image/svg+xml;base64,PHN2Zz4=".into(),
                detail: None,
            },
            UserInput::File {
                file_data: "data:application/pdf;base64,JVBERi0xLjcKZml4dHVyZQolJUVPRg==".into(),
                filename: Some("invoice.pdf".into()),
            },
            UserInput::File {
                file_data: "data:text/plain;base64,SW52b2ljZSBub3Rlcw==".into(),
                filename: None,
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
            .max_tokens(4096)
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
        assert_eq!(
            requests.lock().unwrap().len(),
            1,
            "terminal replay does not resend media"
        );
        if pass == 2 {
            agent
                .prompt("Recall those attachments")
                .await
                .unwrap()
                .result()
                .await
                .unwrap();
        }
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
        2,
        "only the original and the follow-up turn send HTTP requests"
    );
    let blocks = requests[0]["messages"][0]["content"].as_array().unwrap();
    assert_eq!(
        blocks
            .iter()
            .map(|v| v["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "text", "text", "text", "image", "image", "text", "document", "document"
        ]
    );
    assert_eq!(blocks[0]["text"], "first");
    assert_eq!(blocks[2]["text"], "between");
    assert_eq!(blocks[3]["source"]["data"], PNG);
    assert_eq!(blocks[4]["source"], blocks[3]["source"]);
    for note in [&blocks[1], &blocks[5]] {
        assert!(
            note["text"]
                .as_str()
                .unwrap()
                .starts_with("image content omitted"),
            "a remote or unusable image keeps its place as a note: {note}"
        );
    }
    assert_eq!(blocks[6]["source"]["media_type"], "application/pdf");
    assert_eq!(blocks[6]["title"], "invoice.pdf");
    assert_eq!(blocks[7]["source"]["data"], "Invoice notes");
    assert_eq!(
        requests[1]["messages"][0]["content"], requests[0]["messages"][0]["content"],
        "reopened history retains media bytes and order"
    );
    let artifact = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../output/claude-host-integration");
    std::fs::create_dir_all(&artifact).unwrap();
    std::fs::write(
        artifact.join("durable-media-requests.json"),
        serde_json::to_vec_pretty(&*requests).unwrap(),
    )
    .unwrap();
    eprintln!(
        "PASS ordered images+notes+documents; changed+deleted file terminal replay; reopened follow-up history; HTTP requests=2"
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
        .max_tokens(4096)
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

/// An image Claude cannot use is replaced by a note telling the model why,
/// and the rest of the prompt is still sent.
#[tokio::test]
async fn unusable_prompt_images_are_replaced_by_notes() {
    let dir = tempfile::tempdir().unwrap();
    let not_an_image = dir.path().join("fake.png");
    std::fs::write(&not_an_image, b"not an image").unwrap();
    let oversized = dir.path().join("large.png");
    std::fs::File::create(&oversized)
        .unwrap()
        .set_len(64 * 1024 * 1024 + 1)
        .unwrap();
    let (client, requests, _, _, task) = server(false).await;
    let (agent, events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(4096)
        .build()
        .unwrap();
    let local = |path: &std::path::Path| UserInput::LocalImage {
        path: path.to_owned(),
        detail: None,
    };
    let image = |image_url: &str| UserInput::Image {
        image_url: image_url.to_owned(),
        detail: None,
    };
    let prompt = Prompt::content([
        UserInput::Text {
            text: "Describe what arrived.".into(),
        },
        UserInput::ImageFile {
            file_id: "file-synthetic".into(),
            detail: None,
        },
        local(dir.path()),
        local(&not_an_image),
        local(&oversized),
        image("https://example.com/a.png"),
        image("http://example.com/a.png"),
        image("https://user:secret@example.com/a.png"),
        image("data:image/png;base64,garbage"),
        image("data:image/svg+xml;base64,PHN2Zz4="),
        image("data:image/jpeg;base64,iVBORw0KGgo="),
        image(&data()),
    ]);
    let answer = agent.prompt(prompt).await.unwrap().result().await.unwrap();
    assert_eq!(answer.final_message(), "image received");
    agent.shutdown().await.unwrap();
    drop((agent, events));

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let content = requests[0]["messages"][0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 12);
    assert_eq!(content[0]["text"], "Describe what arrived.");
    for note in &content[1..11] {
        assert_eq!(note["type"], "text", "{note}");
        assert!(
            note["text"]
                .as_str()
                .unwrap()
                .starts_with("image content omitted"),
            "{note}"
        );
    }
    for (note, path) in [
        (&content[2], dir.path()),
        (&content[3], not_an_image.as_path()),
    ] {
        assert!(
            note["text"]
                .as_str()
                .unwrap()
                .contains(&path.display().to_string()),
            "a local image's note names its file: {note}"
        );
    }
    assert_eq!(content[11]["source"]["data"], PNG);
    assert!(
        !requests[0].to_string().contains("secret"),
        "URL credentials never reach the provider"
    );
    eprintln!(
        "PASS opaque file ID, nonregular/non-image/oversized local files, remote URLs, garbage/SVG/mislabeled data: 10 notes, 1 image, 1 HTTP request"
    );
    task.abort();
}

#[tokio::test]
async fn audio_and_prompts_beyond_media_limits_fail_before_http() {
    let dir = tempfile::tempdir().unwrap();
    let (client, requests, _, _, task) = server(false).await;
    let (agent, events) = Nanocodex::builder(Claude::new(client, "test"))
        .max_tokens(4096)
        .build()
        .unwrap();
    for item in [
        UserInput::Audio {
            audio_url: "https://example.com/audio.wav".into(),
        },
        UserInput::LocalAudio {
            path: dir.path().join("audio.wav"),
        },
    ] {
        let result = agent.prompt(Prompt::content([item])).await;
        assert!(result.is_err(), "audio must fail before acceptance");
    }
    let many = (0..21).map(|_| UserInput::Image {
        image_url: data(),
        detail: None,
    });
    assert!(agent.prompt(Prompt::content(many)).await.is_err());
    assert!(requests.lock().unwrap().is_empty());
    agent.shutdown().await.unwrap();
    drop((agent, events));
    eprintln!("PASS audio, local audio, and image-count bound: 0 HTTP requests");
    task.abort();
}

/// Delays every durable record read, so an agent reopened on this store is
/// still loading a resumed turn's continuation when that turn accepts a steer.
struct SlowReads(SqliteStore);

impl StateStore for SlowReads {
    fn read_record<'a>(
        &'a mut self,
        state_id: &'a str,
        key: &'a str,
    ) -> StoreFuture<'a, Result<Option<String>, StoreError>> {
        Box::pin(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            self.0.read_record(state_id, key).await
        })
    }

    fn acquire<'a>(
        &'a mut self,
        state_id: &'a str,
        owner_id: OwnerId,
    ) -> StoreFuture<'a, Result<OwnedState, StoreError>> {
        self.0.acquire(state_id, owner_id)
    }

    fn replace<'a>(
        &'a mut self,
        state_id: &'a str,
        owner: &'a OwnerToken,
        expected_revision: u64,
        payload: &'a str,
        records: &'a [StoreRecord],
    ) -> StoreFuture<'a, Result<u64, StoreError>> {
        self.0
            .replace(state_id, owner, expected_revision, payload, records)
    }
}

/// An agent reopened with another model resumes an unfinished turn on the
/// model its frozen requests name, so its image steers are prepared for that
/// model: one retained across the reopen, and one accepted while the turn is
/// still loading its continuation. A steer image Claude cannot use is retained
/// as the note that replaces it, so URL credentials never reach the journal.
#[tokio::test]
async fn recovered_image_steers_keep_the_resolution_of_the_frozen_model() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use std::{io::Cursor, time::Duration};

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("steer.sqlite");
    // Anthropic's example size: kept by the high-resolution tier and reduced
    // to 1456x819 by the standard tier.
    let mut png = Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(1920, 1080)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let steer = Prompt::content([UserInput::Image {
        image_url: format!(
            "data:image/png;base64,{}",
            STANDARD.encode(png.into_inner())
        ),
        detail: None,
    }]);
    let prompt = || PromptRequest::new("original").request_id("steer-operation");
    let (client, requests, started, release, task) = server(true).await;
    let open = async |model: &str, session: DurableSession| {
        Nanocodex::builder(Claude::new(client.clone(), model))
            .max_tokens(4096)
            .durability(session)
            .await
            .unwrap()
            .build()
            .unwrap()
    };

    let session = DurableSession::open(SqliteStore::open(&db).unwrap(), "steer-session")
        .await
        .unwrap();
    let (agent, events) = open("claude-opus-5-5", session).await;
    let turn = agent.prompt(prompt()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    turn.steer_with_id("retained".into(), steer.clone())
        .await
        .unwrap();
    turn.steer_with_id(
        "unusable".into(),
        Prompt::content([UserInput::Image {
            image_url: "https://user:secret@example.com/a.png".into(),
            detail: None,
        }]),
    )
    .await
    .unwrap();

    let session = DurableSession::open(SlowReads(SqliteStore::open(&db).unwrap()), "steer-session")
        .await
        .unwrap();
    let (recovered, recovered_events) = open("claude-haiku-4-5", session).await;
    let resumed = recovered.prompt(prompt()).await.unwrap();
    resumed
        .steer_with_id("during-recovery".into(), steer)
        .await
        .unwrap();
    let answer = tokio::time::timeout(Duration::from_secs(30), resumed.result())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(answer.final_message(), "image received");
    let request = requests.lock().unwrap().last().unwrap().clone();
    assert_eq!(request["model"], "claude-opus-5-5");
    let images: Vec<_> = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .filter(|block| block["type"] == "image")
        .map(|block| {
            let bytes = STANDARD
                .decode(block["source"]["data"].as_str().unwrap())
                .unwrap();
            let image = image::load_from_memory(&bytes).unwrap();
            (image.width(), image.height())
        })
        .collect();
    assert_eq!(
        images,
        [(1920, 1080), (1920, 1080)],
        "the retained and the during-recovery steer keep the frozen model's resolution"
    );
    let notes = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .filter(|block| {
            block["text"]
                .as_str()
                .is_some_and(|text| text.starts_with("image content omitted"))
        })
        .count();
    assert_eq!(notes, 1, "the unusable steer reaches the model as a note");
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let path = entry.unwrap().path();
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            !bytes.windows(6).any(|window| window == b"secret"),
            "{} retains URL credentials",
            path.display()
        );
    }

    release.notify_one();
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result()).await;
    recovered.shutdown().await.unwrap();
    let _ = agent.shutdown().await;
    drop((agent, events, recovered, recovered_events));
    eprintln!(
        "PASS both recovered image steers kept Opus 5.5's resolution after a Haiku 4.5 reopen"
    );
    task.abort();
}
