#![cfg(feature = "claude")]

use nanocodex_agent::{Nanocodex, Origin};
use nanocodex_claude::ClaudeTools;
use nanocodex_claude::{Claude, ClaudeClient};
use nanocodex_subagents::{
    AgentTask, MemorySubagentStore, SubagentStore, SubagentStoreFuture, channel,
    install_claude_tools, start_agent,
};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

#[derive(Default)]
struct RecordingStore {
    inner: MemorySubagentStore,
    writes: Mutex<Vec<String>>,
    saved: Notify,
}
impl SubagentStore for RecordingStore {
    fn load<'a>(
        &'a self,
        root: &'a str,
    ) -> SubagentStoreFuture<'a, std::io::Result<Option<String>>> {
        self.inner.load(root)
    }
    fn save<'a>(
        &'a self,
        root: &'a str,
        payload: String,
    ) -> SubagentStoreFuture<'a, std::io::Result<()>> {
        Box::pin(async move {
            self.inner.save(root, payload.clone()).await?;
            self.writes.lock().unwrap().push(payload);
            self.saved.notify_one();
            Ok(())
        })
    }
    fn record_session<'a>(
        &'a self,
        root: &'a str,
        checkpoint: nanocodex_agent::SessionCheckpoint,
    ) -> SubagentStoreFuture<'a, std::io::Result<()>> {
        self.inner.record_session(root, checkpoint)
    }
}

// Real public spawn, Claude HTTP transport, journal store and restoration. The
// only stub is the external provider, held in its first request across restore.
#[tokio::test]
async fn first_claude_turn_is_recoverable_before_provider_returns() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    tokio::time::timeout(Duration::from_secs(10), async {
        let called = Arc::new(Notify::new());
        let request_called = called.clone();
        let app = axum::Router::new().route("/", axum::routing::post(move || {
            let called = request_called.clone();
            async move {
                called.notify_one();
                std::future::pending::<String>().await
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let store = Arc::new(RecordingStore::default());
        let (registry, _control, _updates) = channel(1);
        registry.set_store(store.clone());
        let tool_registry = registry.clone();
        let handle = Arc::new(Mutex::new(None));
        let capture = handle.clone();
        let client = ClaudeClient::new(reqwest::Client::new(), url, "synthetic");
        let (parent, _events) = Nanocodex::builder(Claude::new(client, "claude-sonnet-4-6"))
            .max_tokens(4096)
            .tools_factory(move |handle| {
                capture.lock().unwrap().get_or_insert_with(|| handle.clone());
                install_claude_tools(ClaudeTools::default(), handle, tool_registry.clone())
            })
            .build().unwrap();
        let root = parent.session_id().to_owned();
        let parent_handle = handle.lock().unwrap().clone().unwrap();
        let child = start_agent(&parent_handle, &registry, &root, AgentTask {
            role: "checkpoint probe".into(), task: "wait for the provider".into(), output_schema: json!({"type":"string"}),
        }).await.unwrap();
        called.notified().await;
        loop {
            let payload = store.load(&root).await.unwrap();
            if payload.as_ref().is_some_and(|s| s.contains("\"turn_in_flight\":true")) { break; }
            store.saved.notified().await;
        }
        // Reconstruct from every admitted journal, including the earliest one.
        let writes = store.writes.lock().unwrap().clone();
        let mut running_restores = 0;
        for payload in writes {
            let copy = MemorySubagentStore::new();
            copy.save(&root, payload.clone()).await.unwrap();
            let (restored, _, _) = channel(1);
            restored.set_store(Arc::new(copy));
            let report = restored.restore(&root).await.unwrap();
            if report.restored == 0 { continue; }
            assert!(report.unrecoverable.is_empty(), "first-turn journal lost Claude child: {payload}");
            if payload.contains("\"turn_in_flight\":true") {
                assert_eq!(report.interrupted, vec![child.agent_id]);
                running_restores += 1;
            }
        }
        assert!(running_restores > 0, "must restore an in-flight journal");
        // The child is also its own durable session: distinct identity,
        // subagent provenance under this root, resumable checkpoint.
        let recorded = loop {
            let ids = store.inner.sessions();
            if !ids.is_empty() { break ids; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(recorded.len(), 1, "one child session: {recorded:?}");
        assert_ne!(recorded[0], root, "child needs its own session ID");
        let session = store.inner.session(&recorded[0]).unwrap();
        session.validate().unwrap();
        assert_eq!(session.lineage().origin, Origin::Subagent);
        assert_eq!(session.lineage().root_session_id, root);
        assert_eq!(session.lineage().parent_session_id.as_deref(), Some(root.as_str()));
        assert_eq!(session.lineage().depth, 1);
        eprintln!("Claude HTTP request held open; every saved child journal restored as interrupted, with no unrecoverable children");
        registry.close(&root, child.agent_id).await.unwrap();
        parent.shutdown().await.unwrap();
        server.abort();
    }).await.expect("checkpoint must not wait for the provider to return");
}
