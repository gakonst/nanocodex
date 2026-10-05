//! Drop witnesses around the real public durable facades and SQLite stores.
use nanocodex::{
    DurableAgentExt, Nanocodex, OpenAi,
    durability::{
        DurableSession, OwnedState, OwnerId, OwnerToken, SqliteStore, StateStore, StoreError,
        StoreFuture, StoreRecord,
    },
};
use std::{sync::Arc, time::Duration};

pub(super) struct WitnessStore {
    pub(super) inner: SqliteStore,
    pub(super) _witness: Arc<()>,
}
impl StateStore for WitnessStore {
    fn read_record<'a>(
        &'a mut self,
        id: &'a str,
        key: &'a str,
    ) -> StoreFuture<'a, Result<Option<String>, StoreError>> {
        self.inner.read_record(id, key)
    }
    fn acquire<'a>(
        &'a mut self,
        id: &'a str,
        owner: OwnerId,
    ) -> StoreFuture<'a, Result<OwnedState, StoreError>> {
        self.inner.acquire(id, owner)
    }
    fn replace<'a>(
        &'a mut self,
        id: &'a str,
        owner: &'a OwnerToken,
        revision: u64,
        payload: &'a str,
        records: &'a [StoreRecord],
    ) -> StoreFuture<'a, Result<u64, StoreError>> {
        self.inner.replace(id, owner, revision, payload, records)
    }
}

async fn witness(claude: bool, shutdown: bool) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let path = std::env::temp_dir().join(format!(
        "native-owner-drop-{}.sqlite",
        nanocodex::agent::session::SessionId::new()
    ));
    let owned = Arc::new(());
    let weak = Arc::downgrade(&owned);
    let state = DurableSession::open(
        WitnessStore {
            inner: SqliteStore::open(&path).unwrap(),
            _witness: owned,
        },
        "drop-root",
    )
    .await
    .unwrap();
    let (agent, events) = if claude {
        #[cfg(feature = "claude")]
        {
            let client = nanocodex::claude::ClaudeClient::new(
                reqwest::Client::new(),
                "http://127.0.0.1:1/v1/messages",
                "synthetic-key",
            );
            Nanocodex::builder(nanocodex::Claude::new(client, "claude-sonnet-5-5"))
                .durability(state)
                .await
                .unwrap()
                .build()
                .unwrap()
        }
        #[cfg(not(feature = "claude"))]
        {
            unreachable!()
        }
    } else {
        let backend = OpenAi::builder("synthetic-key")
            .api_base_url("http://127.0.0.1:1")
            .websocket_warmup(false)
            .build()
            .unwrap();
        Nanocodex::builder(backend)
            .durability(state)
            .await
            .unwrap()
            .build()
            .unwrap()
    };
    agent.ready().await.unwrap();
    assert!(weak.upgrade().is_some());
    let retained = agent.clone();
    drop(agent);
    assert!(
        weak.upgrade().is_some(),
        "a live facade clone must retain its owner"
    );
    let agent = retained;
    if shutdown {
        agent.shutdown().await.unwrap();
    }
    drop((agent, events));
    let dropped = tokio::time::timeout(Duration::from_secs(2), async {
        while weak.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok();
    let _ = std::fs::remove_file(path);
    assert!(
        dropped,
        "public durable owner leaked its store: claude={claude}, shutdown={shutdown}"
    );
}

#[tokio::test]
async fn native_idle_shutdown_releases_durable_owner() {
    witness(false, true).await;
}
#[tokio::test]
async fn native_idle_drop_releases_durable_owner() {
    witness(false, false).await;
}
#[cfg(feature = "claude")]
#[tokio::test]
async fn claude_idle_shutdown_releases_durable_owner() {
    witness(true, true).await;
}
#[cfg(feature = "claude")]
#[tokio::test]
async fn claude_idle_drop_releases_durable_owner() {
    witness(true, false).await;
}
