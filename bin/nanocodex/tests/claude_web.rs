//! Public API and shipped CLI acceptance with real HTTPS pages and synthetic
//! Messages/SSE. The production executable has no loopback transport override.
#[path = "support/local_cli.rs"]
mod local_cli;
use local_cli::local_cli;

#[path = "../src/config/claude/web.rs"]
mod web;

use axum::{Json, Router, routing::post};
use nanocodex::claude_tools::web::{ApprovedWebFetchSource, WebFetchRequest};
use nanocodex::{Claude, Nanocodex, claude::ClaudeClient};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::net::TcpListener;

fn sse(block: Value) -> String {
    let tool = block["type"] == "tool_use";
    let start = if tool {
        json!({"type":"tool_use","id":block["id"],"name":block["name"],"input":{}})
    } else {
        block.clone()
    };
    let mut events = vec![
        json!({"type":"message_start","message":{"id":"fixture","role":"assistant","model":"fixture","content":[],"usage":{"input_tokens":1,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":start}),
    ];
    if tool {
        events.push(json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":block["input"].to_string()}}));
    }
    events.extend([json!({"type":"content_block_stop","index":0}),json!({"type":"message_delta","delta":{"stop_reason":if tool {"tool_use"} else {"end_turn"}},"usage":{"output_tokens":1}}),json!({"type":"message_stop"})]);
    events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect()
}
fn artifact(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../output/claude-web")
        .join(format!("{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    path
}
fn last_result(body: &Value) -> &Value {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .flat_map(|m| m["content"].as_array().into_iter().flatten())
        .find(|b| b["type"] == "tool_result")
        .unwrap()
}
struct Fixture(std::process::Child);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn real_https_pages_redirect_policy_and_auxiliary_messages() {
    nanocodex::oai::transport::install_default_rustls_crypto_provider();
    let artifact = artifact("https-api");
    let cert = artifact.join("cert.pem");
    let key = artifact.join("key.pem");
    let status = std::process::Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=pages.fixture.test",
            "-addext",
            "subjectAltName=DNS:pages.fixture.test",
            "-addext",
            "basicConstraints=critical,CA:FALSE",
            "-addext",
            "extendedKeyUsage=serverAuth",
            "-keyout",
        ])
        .arg(&key)
        .arg("-out")
        .arg(&cert)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let mut fixture = Fixture(
        std::process::Command::new("python3")
            .arg(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../scripts/tests/claude-web-pages-fixture.py"),
            )
            .arg(&cert)
            .arg(&key)
            .arg(artifact.join("pages.jsonl"))
            .stdout(Stdio::piped())
            .stderr(Stdio::from(
                std::fs::File::create(artifact.join("pages-stderr.log")).unwrap(),
            ))
            .spawn()
            .unwrap(),
    );
    let mut port = String::new();
    BufReader::new(fixture.0.stdout.take().unwrap())
        .read_line(&mut port)
        .unwrap();
    let port: u16 = port.trim().parse().unwrap();
    let origin = format!("https://pages.fixture.test:{port}");
    let source = Arc::new(web::PublicWebFetch {
        fixture: Some((
            "pages.fixture.test".into(),
            ([127, 0, 0, 1], port).into(),
            reqwest::Certificate::from_pem(&std::fs::read(&cert).unwrap()).unwrap(),
        )),
    });
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = requests.clone();
    let count = Arc::new(Mutex::new(0usize));
    let root_count = count.clone();
    let url = origin.clone();
    let app = Router::new().route("/v1/messages",post(move |Json(body):Json<Value>| {
        let log=log.clone(); let count=root_count.clone(); let url=url.clone();
        async move {
            let summary=body["model"]=="claude-haiku-4-5-20251001";
            log.lock().unwrap().push(body);
            let block=if summary { json!({"type":"text","text":"Synthetic page title"}) } else {
                let n={let mut n=count.lock().unwrap(); let i=*n; *n+=1; i};
                let paths=["/start","/private","/downgrade","/large","/binary","/loop","/slow"];
                if n<paths.len() { json!({"type":"tool_use","id":format!("fetch-{n}"),"name":"WebFetch","input":{"url":format!("{url}{}",paths[n]),"prompt":"Read title"}}) }
                else { json!({"type":"text","text":"https-page-journey-complete"}) }
            };
            ([("content-type","text/event-stream")],sse(block))
        }
    }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClaudeClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        endpoint,
        "synthetic",
    );
    let (agent, _) = Nanocodex::builder(Claude::new(client, "fixture"))
        .max_tokens(1024)
        .web_fetch_with_source(source, false)
        .build()
        .unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(40), async {
        agent
            .prompt("Exercise actual TLS page fetch")
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
    })
    .await
    .unwrap();
    let requests = requests.lock().unwrap().clone();
    std::fs::write(
        artifact.join("messages.json"),
        serde_json::to_vec_pretty(&requests).unwrap(),
    )
    .unwrap();
    assert_eq!(reply.final_message(), "https-page-journey-complete");
    let roots: Vec<_> = requests
        .iter()
        .filter(|r| r["model"] == "fixture")
        .collect();
    assert_eq!(roots.len(), 8);
    for n in [1, 4] {
        assert_ne!(last_result(roots[n])["is_error"], true);
        assert!(
            last_result(roots[n])["content"]
                .as_str()
                .unwrap()
                .contains("Synthetic page title")
        );
    }
    assert!(
        last_result(roots[1])["content"]
            .as_str()
            .unwrap()
            .ends_with(&format!("Source: {origin}/final"))
    );
    for n in [2, 3, 5, 6, 7] {
        assert_eq!(last_result(roots[n])["is_error"], true, "request {n}");
    }
    let summaries: Vec<_> = requests
        .iter()
        .filter(|r| r["model"] == "claude-haiku-4-5-20251001")
        .collect();
    assert_eq!(summaries.len(), 2);
    for summary in &summaries {
        assert!(summary.get("tools").is_none());
        assert!(
            summary["messages"][0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("Synthetic page title")
        );
    }
    assert!(
        summaries[1]["messages"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("[Page capture truncated]")
    );
    assert!(
        summaries[1]["messages"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .len()
            < 129 * 1024
    );
    let pages: Vec<Value> = std::fs::read_to_string(artifact.join("pages.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(pages.len(), 13);
    assert!(!pages.iter().any(|p| p["path"] == "/secret"));
    for page in &pages {
        let headers = page["headers"].as_object().unwrap();
        for name in headers.keys() {
            assert!(
                !["cookie", "authorization", "proxy-authorization", "referer"]
                    .contains(&name.to_lowercase().as_str())
            );
        }
    }
    // Exercise the production capability at its public trait boundary. None of
    // these paths may hit the local TLS server or inherit the fixture grant.
    let production = web::PublicWebFetch::new();
    let mut denied = Vec::new();
    for url in [
        format!("https://127.0.0.1:{port}/secret"),
        format!("https://localhost:{port}/secret"),
        format!("https://[::1]:{port}/secret"),
        "http://example.org/".into(),
        "https://user:pass@example.org/".into(),
        "https://169.254.169.254/".into(),
    ] {
        let error = production
            .fetch_source(WebFetchRequest {
                url: url.clone(),
                prompt: "read".into(),
                max_output_bytes: 1024,
            })
            .await
            .unwrap_err();
        denied.push(json!({"url":url,"error":error}));
    }
    assert_eq!(
        std::fs::read_to_string(artifact.join("pages.jsonl"))
            .unwrap()
            .lines()
            .count(),
        13
    );
    std::fs::write(
        artifact.join("denials.json"),
        serde_json::to_vec_pretty(&denied).unwrap(),
    )
    .unwrap();
    std::fs::write(artifact.join("outcome.txt"),"PASS: public Claude API fetched actual TLS pages, followed relative redirect, preserved final citation, bounded large UTF-8 page, refused private/downgrade/loop/binary targets, timed out stalled body, sent no cookies/credentials/referrer. Default source rejects loopback, metadata, HTTP and credentials.\nCommand: CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 cargo +1.97.0 test -p nanocodex-bin --test claude_web -- --nocapture\n").unwrap();
    eprintln!("evidence: {}", artifact.display());
    server.abort();
    drop(fixture);
    std::fs::remove_file(key).unwrap();
}

#[tokio::test]
async fn shipped_cli_native_search_fetch_opt_in_and_default_denial() {
    let artifact = artifact("shipped-cli");
    let workspace = artifact.join("workspace");
    std::fs::create_dir_all(workspace.join("home")).unwrap();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = requests.clone();
    let root_count = Arc::new(Mutex::new(0usize));
    let app=Router::new().route("/v1/messages",post(move |Json(body):Json<Value>| {
        let log=log.clone(); let count=root_count.clone();
        async move {
            let nested=body["tools"].as_array().is_some_and(|t|t.len()==1 && t[0]["name"]=="web_search");
            log.lock().unwrap().push(body);
            let block=if nested { json!({"type":"text","text":"Synthetic search answer","citations":[{"type":"web_search_result_location","url":"https://example.org/source","encrypted_index":"fixture"}]}) } else {
                let n={let mut n=count.lock().unwrap();let i=*n;*n+=1;i};
                match n {
                    0=>json!({"type":"tool_use","id":"search","name":"WebSearch","input":{"query":"synthetic public source","allowed_domains":["example.org"]}}),
                    1=>json!({"type":"tool_use","id":"fetch","name":"WebFetch","input":{"url":"https://localhost.localdomain/private","prompt":"read"}}),
                    3=>json!({"type":"tool_use","id":"disabled-catalog","name":"exec","input":{"code":"if (ALL_TOOLS.some(tool => tool.name === 'WebSearch' || tool.name === 'WebFetch')) throw Error('disabled web capability available'); text('disabled-catalog-verified');"}}),
                    _=>json!({"type":"text","text":"native-web-cli-complete"}),
                }
            };
            let block = if block["type"] == "tool_use" && block["name"] != "exec" {
                json!({"type":"tool_use", "id":block["id"], "name":"exec", "input":{"code":format!("try {{ text(await tools.{}({})); }} catch (error) {{ text({{error: error.message || String(error)}}); }}", block["name"].as_str().unwrap(), block["input"])}})
            } else { block };
            ([("content-type","text/event-stream")],sse(block))
        }
    }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base = |enabled: &str| {
        let mut cmd = tokio::process::Command::new(local_cli());
        cmd.arg("run")
            .current_dir(&workspace)
            .env_clear()
            .env("HOME", workspace.join("home"))
            .env("CODEX_HOME", workspace.join("home/codex"))
            .env("PATH", "/usr/bin:/bin")
            .env("NANOCODEX_COMPUTER", "off")
            .args([
                "--claude",
                "--model",
                "claude-sonnet-5-5",
                "--thinking",
                "medium",
                "--claude-api-key",
                "synthetic",
                "--claude-messages-url",
                &endpoint,
                "--rollouts",
                "false",
                "--browser=none",
                "--mcp-defaults",
                "false",
                "--mcp-codex-config",
                "false",
                "--web-search",
                enabled,
                "--image-generation",
                "false",
                "--memory",
                "false",
                "--subagents",
                "false",
                "Exercise web tools",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        cmd
    };
    let mut enabled = base("true");
    std::fs::write(artifact.join("command.txt"), format!("{enabled:?}\n")).unwrap();
    let output = tokio::time::timeout(Duration::from_secs(40), enabled.output())
        .await
        .unwrap()
        .unwrap();
    std::fs::write(artifact.join("stdout.txt"), &output.stdout).unwrap();
    std::fs::write(artifact.join("stderr.txt"), &output.stderr).unwrap();
    let captured = requests.lock().unwrap().clone();
    std::fs::write(
        artifact.join("messages.json"),
        serde_json::to_vec_pretty(&captured).unwrap(),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("native-web-cli-complete"));
    assert_eq!(captured.len(), 4);
    let names: Vec<_> = captured[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert_eq!(names, vec!["exec", "wait"]);
    assert_eq!(
        captured[1]["tools"][0]["allowed_domains"],
        json!(["example.org"])
    );
    assert!(
        last_result(&captured[2])["content"]
            .to_string()
            .contains("Source: https://example.org/source")
    );
    let denial = last_result(&captured[3])["content"].to_string();
    assert!(
        denial.contains("error") && denial.contains("approved WebFetch source failed"),
        "{denial}"
    );
    let output = tokio::time::timeout(Duration::from_secs(40), base("false").output())
        .await
        .unwrap()
        .unwrap();
    assert!(output.status.success());
    let captured = requests.lock().unwrap();
    let disabled = captured.last().unwrap();
    let names: Vec<_> = disabled["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert_eq!(names, vec!["exec", "wait"]);
    let result = last_result(disabled);
    assert_ne!(result["is_error"], true);
    assert!(
        result["content"]
            .to_string()
            .contains("disabled-catalog-verified")
    );
    std::fs::write(
        artifact.join("disabled-request.json"),
        serde_json::to_vec_pretty(disabled).unwrap(),
    )
    .unwrap();
    std::fs::write(artifact.join("outcome.txt"),"PASS: shipped CLI --web-search true registers native WebSearch/WebFetch, search makes independent Messages request with domain filters and source citation, private fetch returns tool error; false removes both tools.\nCommand: CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 cargo +1.97.0 test -p nanocodex-bin --test claude_web -- --nocapture\n").unwrap();
    eprintln!("evidence: {}", artifact.display());
    server.abort();
}
