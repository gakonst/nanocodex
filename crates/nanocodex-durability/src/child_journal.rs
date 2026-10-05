//! Fenced child-tree metadata on the same host store as durable execution.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{Error, OwnerId, OwnerToken, StateStore, StoreError, StoreRecord};

// JSON serialization cannot begin with NUL. Legacy raw JSON therefore cannot
// impersonate an envelope, even if an application uses our field/tag names.
const HEAD_PREFIX: &str = "\0nanocodex:child-journal:1\n";
const KEY_PREFIX: &str = "child-journal/1/";
const INLINE_BYTES: usize = 8 * 1024;
const RECORD_BYTES: usize = 64 * 1024;
const READ_BATCH: usize = 16;
const MAX_DEPTH: usize = 128;

// Inline is an explicit escape for application JSON. Its contents are never
// interpreted as a reference, so user objects cannot collide with this format.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", deny_unknown_fields)]
enum Node {
    Inline(Value),
    Object(BTreeMap<String, Self>),
    Array(Vec<Self>),
    Record(String),
}

/// A child-tree journal. A failed write or hydration poisons this owner: reopen
/// to reconcile authoritative state before admitting work or reporting receipts.
///
/// Heads contain only a version and immutable root key. Oversized JSON subtrees
/// and child snapshots, results and mailbox data are staged as content-addressed
/// records, each at most 64 KiB. Existing raw JSON heads remain readable; the next
/// commit migrates them atomically. Published record keys are retained by this
/// owner so unchanged content is not sent again on subsequent commits.
pub struct ChildJournal {
    store: Box<dyn StateStore + Send>,
    state_id: String,
    owner: OwnerToken,
    revision: u64,
    payload: Option<String>,
    published: BTreeSet<String>,
    poisoned: bool,
}

impl ChildJournal {
    /// Acquires the host's existing ownership/revision fence for this tree.
    pub async fn open(
        #[cfg(not(target_family = "wasm"))] store: impl StateStore + 'static,
        #[cfg(target_family = "wasm")] store: impl StateStore + Send + 'static,
        root: &str,
    ) -> crate::Result<Self> {
        let mut store = Box::new(store);
        let state_id = format!("{root}/children");
        let acquired = store.acquire(&state_id, OwnerId::new()).await?;
        Ok(Self {
            store,
            state_id,
            owner: acquired.owner,
            revision: acquired.state.revision,
            payload: acquired.state.payload,
            published: BTreeSet::new(),
            poisoned: false,
        })
    }

    /// Hydrates and decodes the committed child tree without acquiring an owner.
    /// Missing records, invalid manifests and checksum failures fail closed.
    pub async fn load<T: DeserializeOwned>(&mut self) -> crate::Result<Option<T>> {
        if self.poisoned {
            return Err(StoreError::Fenced.into());
        }
        let result = self.load_inner().await;
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    async fn load_inner<T: DeserializeOwned>(&mut self) -> crate::Result<Option<T>> {
        let Some(payload) = self.payload.as_deref() else {
            return Ok(None);
        };
        let Some(key) = payload.strip_prefix(HEAD_PREFIX) else {
            // NUL also makes an unknown future envelope fail JSON decoding.
            return serde_json::from_str(payload)
                .map(Some)
                .map_err(|source| Error::Decode {
                    revision: self.revision,
                    source,
                });
        };
        validate_key(key)?;
        let root = key.to_owned();
        let mut records = BTreeMap::new();
        let json = self.load_blob(&root, &mut records).await?;
        let mut node: Node = serde_json::from_str(&json).map_err(|source| Error::Decode {
            revision: self.revision,
            source,
        })?;
        // Hydrate one bounded frontier at a time. The format explicitly encodes
        // structure; it never walks or replaces reference-like user JSON.
        for _ in 0..MAX_DEPTH {
            let mut keys = BTreeSet::new();
            references(&node, &mut keys, 0)?;
            if keys.is_empty() {
                let value = into_value(node, 0)?;
                let result = serde_json::from_value(value).map_err(|source| Error::Decode {
                    revision: self.revision,
                    source,
                })?;
                self.published.extend(records.into_keys());
                return Ok(Some(result));
            }
            let mut loaded = BTreeMap::new();
            let keys: Vec<_> = keys.into_iter().collect();
            for page in keys.chunks(READ_BATCH) {
                self.fetch(page, &mut records).await?;
                for key in page {
                    let json = self.load_blob(key, &mut records).await?;
                    let child = serde_json::from_str(&json).map_err(|source| Error::Decode {
                        revision: self.revision,
                        source,
                    })?;
                    loaded.insert(key.clone(), child);
                }
            }
            expand(&mut node, &loaded, 0)?;
        }
        Err(invalid("child journal reference depth exceeded"))
    }

    async fn fetch(
        &mut self,
        keys: &[String],
        records: &mut BTreeMap<String, String>,
    ) -> crate::Result<()> {
        let missing: Vec<_> = keys
            .iter()
            .filter(|key| !records.contains_key(*key))
            .cloned()
            .collect();
        for page in missing.chunks(READ_BATCH) {
            for key in page {
                validate_key(key)?;
            }
            let values = self.store.read_records(&self.state_id, page).await?;
            if values.len() != page.len() {
                return Err(invalid("child journal record batch length mismatch"));
            }
            for (key, value) in page.iter().zip(values) {
                let value =
                    value.ok_or_else(|| invalid(format!("missing child journal record {key}")))?;
                if value.len() > RECORD_BYTES || record_key(&value) != *key {
                    return Err(invalid(format!(
                        "child journal record checksum or size mismatch {key}"
                    )));
                }
                records.insert(key.clone(), value);
            }
        }
        Ok(())
    }

    async fn load_blob(
        &mut self,
        key: &str,
        records: &mut BTreeMap<String, String>,
    ) -> crate::Result<String> {
        // Stack order preserves chunk order. Manifests are themselves bounded
        // records, including when a single result needs multiple manifest levels.
        let mut pending = vec![(key.to_owned(), 0)];
        let mut json = String::new();
        while !pending.is_empty() {
            let start = pending.len().saturating_sub(READ_BATCH);
            let keys: Vec<_> = pending[start..]
                .iter()
                .map(|(key, _)| key.clone())
                .collect();
            self.fetch(&keys, records).await?;
            let (key, depth) = pending.pop().expect("nonempty record stack");
            if depth >= MAX_DEPTH {
                return Err(invalid("child journal chunk depth exceeded"));
            }
            let record = records.get(&key).expect("fetched record");
            if let Some(chunk) = record.strip_prefix('=') {
                json.push_str(chunk);
            } else if let Some(manifest) = record.strip_prefix('+') {
                let children: Vec<String> =
                    serde_json::from_str(manifest).map_err(|source| Error::Decode {
                        revision: self.revision,
                        source,
                    })?;
                if children.is_empty() {
                    return Err(invalid("empty child journal chunk manifest"));
                }
                for child in children.into_iter().rev() {
                    validate_key(&child)?;
                    pending.push((child, depth + 1));
                }
            } else {
                return Err(invalid("invalid child journal record encoding"));
            }
        }
        Ok(json)
    }

    /// Commits metadata, immutable records and the owner/revision fence in the
    /// same store transaction before its observable receipt. A failed or uncertain
    /// replace never updates the published-key cache and poisons this owner.
    pub async fn commit(&mut self, state: &impl Serialize) -> crate::Result<()> {
        if self.poisoned {
            return Err(StoreError::Fenced.into());
        }
        let value = serde_json::to_value(state)?;
        let mut staged = BTreeMap::new();
        let root = stage(value, true, 0, &self.published, &mut staged)?;
        let Node::Record(key) = root else {
            return Err(invalid("child journal staging did not produce a root"));
        };
        let payload = format!("{HEAD_PREFIX}{key}");
        let records: Vec<_> = staged
            .into_iter()
            .map(|(key, value)| StoreRecord { key, value })
            .collect();
        match self
            .store
            .replace(
                &self.state_id,
                &self.owner,
                self.revision,
                &payload,
                &records,
            )
            .await
        {
            Ok(revision) => {
                self.revision = revision;
                self.payload = Some(payload);
                self.published
                    .extend(records.into_iter().map(|record| record.key));
                Ok(())
            }
            Err(error) => {
                self.poisoned = true;
                Err(error.into())
            }
        }
    }
}

fn stage(
    value: Value,
    separate: bool,
    depth: usize,
    published: &BTreeSet<String>,
    records: &mut BTreeMap<String, String>,
) -> crate::Result<Node> {
    if depth >= MAX_DEPTH {
        return Err(invalid("child journal JSON depth exceeded"));
    }
    if !separate && serde_json::to_string(&value)?.len() <= INLINE_BYTES {
        return Ok(Node::Inline(value));
    }
    let node = match value {
        Value::Object(fields) => {
            let mut children = BTreeMap::new();
            for (name, value) in fields {
                let separate = matches!(
                    name.as_str(),
                    "sessions"
                        | "calls"
                        | "messages"
                        | "snapshot"
                        | "history"
                        | "mailbox"
                        | "submitted_output"
                        | "last_output"
                        | "result"
                        | "output"
                        | "host_context"
                        | "execution"
                );
                children.insert(name, stage(value, separate, depth + 1, published, records)?);
            }
            Node::Object(children)
        }
        Value::Array(values) => {
            let children = values
                .into_iter()
                .map(|value| stage(value, false, depth + 1, published, records))
                .collect::<crate::Result<_>>()?;
            Node::Array(children)
        }
        value => Node::Inline(value),
    };
    let json = serde_json::to_string(&node)?;
    Ok(Node::Record(stage_blob(&json, published, records)?))
}

fn stage_blob(
    json: &str,
    published: &BTreeSet<String>,
    records: &mut BTreeMap<String, String>,
) -> crate::Result<String> {
    if json.len() < RECORD_BYTES {
        return Ok(publish(format!("={json}"), published, records));
    }
    let mut keys = Vec::new();
    let mut offset = 0;
    while offset < json.len() {
        let mut end = (offset + RECORD_BYTES - 1).min(json.len());
        while !json.is_char_boundary(end) {
            end -= 1;
        }
        keys.push(publish(
            format!("={}", &json[offset..end]),
            published,
            records,
        ));
        offset = end;
    }
    // Every key has the same ASCII length. Account for JSON quotes, commas,
    // brackets and the manifest marker before grouping a manifest page.
    let manifest_entries = (RECORD_BYTES - 3) / (KEY_PREFIX.len() + 64 + 3);
    while keys.len() > 1 {
        keys = keys
            .chunks(manifest_entries)
            .map(|page| {
                Ok(publish(
                    format!("+{}", serde_json::to_string(page)?),
                    published,
                    records,
                ))
            })
            .collect::<crate::Result<_>>()?;
    }
    keys.pop()
        .ok_or_else(|| invalid("empty child journal content"))
}

fn publish(
    value: String,
    published: &BTreeSet<String>,
    records: &mut BTreeMap<String, String>,
) -> String {
    let key = record_key(&value);
    if !published.contains(&key) {
        records.entry(key.clone()).or_insert(value);
    }
    key
}

fn record_key(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut key = String::with_capacity(KEY_PREFIX.len() + 64);
    key.push_str(KEY_PREFIX);
    for byte in Sha256::digest(value.as_bytes()) {
        key.push(HEX[(byte >> 4) as usize] as char);
        key.push(HEX[(byte & 15) as usize] as char);
    }
    key
}

fn validate_key(key: &str) -> crate::Result<()> {
    let hash = key
        .strip_prefix(KEY_PREFIX)
        .ok_or_else(|| invalid("invalid child journal record key"))?;
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid("invalid child journal record key"));
    }
    Ok(())
}

fn references(node: &Node, keys: &mut BTreeSet<String>, depth: usize) -> crate::Result<()> {
    if depth >= MAX_DEPTH {
        return Err(invalid("child journal structure depth exceeded"));
    }
    match node {
        Node::Record(key) => {
            validate_key(key)?;
            keys.insert(key.clone());
        }
        Node::Object(fields) => {
            for node in fields.values() {
                references(node, keys, depth + 1)?;
            }
        }
        Node::Array(values) => {
            for node in values {
                references(node, keys, depth + 1)?;
            }
        }
        Node::Inline(_) => {}
    }
    Ok(())
}

fn expand(node: &mut Node, loaded: &BTreeMap<String, Node>, depth: usize) -> crate::Result<()> {
    if depth >= MAX_DEPTH {
        return Err(invalid("child journal structure depth exceeded"));
    }
    match node {
        Node::Record(key) => {
            // Repeated references need independent owned structures.
            let value = loaded
                .get(key)
                .ok_or_else(|| invalid("unresolved child journal record"))?;
            *node = value.clone();
        }
        Node::Object(fields) => {
            for node in fields.values_mut() {
                expand(node, loaded, depth + 1)?;
            }
        }
        Node::Array(values) => {
            for node in values {
                expand(node, loaded, depth + 1)?;
            }
        }
        Node::Inline(_) => {}
    }
    Ok(())
}

fn into_value(node: Node, depth: usize) -> crate::Result<Value> {
    if depth >= MAX_DEPTH {
        return Err(invalid("child journal structure depth exceeded"));
    }
    match node {
        Node::Inline(value) => Ok(value),
        Node::Object(fields) => Ok(Value::Object(
            fields
                .into_iter()
                .map(|(key, node)| Ok((key, into_value(node, depth + 1)?)))
                .collect::<crate::Result<_>>()?,
        )),
        Node::Array(values) => Ok(Value::Array(
            values
                .into_iter()
                .map(|node| into_value(node, depth + 1))
                .collect::<crate::Result<_>>()?,
        )),
        Node::Record(_) => Err(invalid("unresolved child journal record")),
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidState(message.into())
}
