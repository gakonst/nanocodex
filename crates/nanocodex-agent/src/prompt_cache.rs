use std::{collections::HashMap, sync::Arc};

use nanocodex_oai_api::{MODEL, responses::RequestProfile};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, OnceCell};

use crate::{NanocodexError, Result};

type PrefixFingerprint = [u8; 32];
type WarmupEntry = Arc<OnceCell<()>>;
type WarmupEntries = HashMap<PrefixFingerprint, WarmupEntry>;

#[derive(Clone)]
pub(crate) struct ModelPromptCache {
    key: Arc<str>,
    shared: Option<SharedPromptCache>,
}

#[derive(Clone, Default)]
pub(crate) struct SharedPromptCache {
    entries: Arc<Mutex<WarmupEntries>>,
}

impl ModelPromptCache {
    pub(crate) const fn new(key: Arc<str>, shared: Option<SharedPromptCache>) -> Self {
        Self { key, shared }
    }

    pub(crate) fn key(&self) -> &str {
        &self.key
    }

    pub(crate) const fn shared(&self) -> Option<&SharedPromptCache> {
        self.shared.as_ref()
    }
}

impl SharedPromptCache {
    pub(crate) async fn entry(&self, profile: &RequestProfile) -> Result<WarmupEntry> {
        let fingerprint = prefix_fingerprint(profile)?;
        let mut entries = self.entries.lock().await;
        Ok(Arc::clone(
            entries.entry(fingerprint).or_insert_with(Arc::default),
        ))
    }
}

fn prefix_fingerprint(profile: &RequestProfile) -> Result<PrefixFingerprint> {
    let encoded = serde_json::to_vec(&(MODEL, profile.prompt_cache_key(), profile.prefix()))
        .map_err(NanocodexError::SerializePromptPrefix)?;
    Ok(Sha256::digest(encoded).into())
}
