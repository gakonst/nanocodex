//! Persistent model-context sequences. Pages reference immutable message records;
//! a new model boundary publishes only new messages and changed tail pages.

use crate::{EncodedPayload, Result, StoreRecord, session::DurableOwner};
use nanocodex_agent::{
    execution::ExecutionContinuation,
    session::{SessionSnapshot, SessionSnapshotHead},
};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::collections::HashSet;

#[derive(Clone, Copy)]
pub(crate) enum Reader<'a> {
    Owner(&'a DurableOwner),
    Session(&'a crate::DurableSession),
}
impl<'a> From<&'a DurableOwner> for Reader<'a> {
    fn from(value: &'a DurableOwner) -> Self {
        Self::Owner(value)
    }
}
impl<'a> From<&'a crate::DurableSession> for Reader<'a> {
    fn from(value: &'a crate::DurableSession) -> Self {
        Self::Session(value)
    }
}
impl Reader<'_> {
    async fn load_payloads(self, payloads: Vec<EncodedPayload>) -> Result<Vec<EncodedPayload>> {
        match self {
            Self::Owner(owner) => owner.load_payloads(payloads).await,
            Self::Session(session) => session.resolve_many(payloads).await,
        }
    }

    async fn load_payload(self, payload: EncodedPayload) -> Result<EncodedPayload> {
        match self {
            Self::Owner(owner) => owner.load_payload(payload).await,
            Self::Session(session) => session.resolve(&payload).await,
        }
    }
}

const PAGE_ITEMS: usize = 64;

#[derive(Clone, Default, Deserialize, Serialize)]
struct Sequence {
    tail: Option<EncodedPayload>,
}

#[derive(Deserialize, Serialize)]
struct Page {
    previous: Option<EncodedPayload>,
    items: Vec<EncodedPayload>,
}

#[derive(Deserialize, Serialize)]
struct Continuation {
    state: Box<RawValue>,
    history: Sequence,
    prefix: Sequence,
}

/// Prepared immutable writes. The cache is advanced only after a successful commit.
pub(crate) struct Prepared {
    pub(crate) payload: EncodedPayload,
    pub(crate) keys: HashSet<String>,
}

struct Writer<'a> {
    known: &'a HashSet<String>,
    keys: HashSet<String>,
    records: Vec<StoreRecord>,
}

impl<'a> Writer<'a> {
    fn new(known: &'a HashSet<String>) -> Self {
        Self {
            known,
            keys: HashSet::new(),
            records: Vec::new(),
        }
    }

    fn record<T: Serialize>(&mut self, value: &T) -> Result<EncodedPayload> {
        let mut payload = EncodedPayload::encode(value)?;
        let key = payload.key.to_string();
        if self.keys.insert(key.clone()) && !self.known.contains(&key) {
            payload.stage(&mut self.records);
        }
        // Page references never keep message bodies resident.
        Ok(payload.reference())
    }

    fn sequence<T: Serialize>(&mut self, items: &[T]) -> Result<Sequence> {
        let mut previous = None;
        for chunk in items.chunks(PAGE_ITEMS) {
            let mut values = Vec::with_capacity(chunk.len());
            for item in chunk {
                values.push(self.record(item)?);
            }
            previous = Some(self.record(&Page {
                previous,
                items: values,
            })?);
        }
        Ok(Sequence { tail: previous })
    }

    fn finish<T: Serialize>(self, value: &T) -> Result<Prepared> {
        Ok(Prepared {
            payload: EncodedPayload::encode(value)?.with_records(self.records),
            keys: self.keys,
        })
    }
}

pub(crate) fn prepare_continuation(
    value: ExecutionContinuation,
    known: &HashSet<String>,
) -> Result<Prepared> {
    let mut writer = Writer::new(known);
    let history = writer.sequence(&value.history)?;
    let prefix = writer.sequence(&value.prefix)?;
    let state = RawValue::from_string(value.state_json).map_err(crate::Error::InvalidPayload)?;
    writer.finish(&Continuation {
        state,
        history,
        prefix,
    })
}

async fn load_sequence<T: serde::de::DeserializeOwned>(
    owner: Reader<'_>,
    sequence: Sequence,
    keys: &mut HashSet<String>,
) -> Result<Vec<T>> {
    let mut pages = Vec::new();
    let mut next = sequence.tail;
    while let Some(reference) = next {
        keys.insert(reference.key.to_string());
        let page: Page = owner.load_payload(reference).await?.decode()?;
        next = page.previous;
        pages.push(page.items);
    }
    let mut items = Vec::new();
    for page in pages.into_iter().rev() {
        for chunk in page.chunks(16) {
            for reference in chunk {
                keys.insert(reference.key.to_string());
            }
            for payload in owner.load_payloads(chunk.to_vec()).await? {
                items.push(payload.decode()?);
            }
        }
    }
    Ok(items)
}

pub(crate) async fn load_continuation(
    owner: Reader<'_>,
    payload: EncodedPayload,
) -> Result<(ExecutionContinuation, HashSet<String>)> {
    let value: Continuation = payload.decode()?;
    let mut keys = HashSet::new();
    let continuation = ExecutionContinuation {
        state_json: value.state.get().to_owned(),
        history: load_sequence(owner, value.history, &mut keys).await?,
        prefix: load_sequence(owner, value.prefix, &mut keys).await?,
    };
    Ok((continuation, keys))
}

#[derive(Deserialize, Serialize)]
pub(crate) struct Snapshot {
    head: SessionSnapshotHead,
    history: Sequence,
    prefix: Option<Sequence>,
}

pub(crate) fn prepare_snapshot(
    value: SessionSnapshot,
    known: &HashSet<String>,
) -> Result<Prepared> {
    let (head, history, prefix) = value.into_context_parts();
    let mut writer = Writer::new(known);
    let history = writer.sequence(&history)?;
    let prefix = prefix.map(|items| writer.sequence(&items)).transpose()?;
    writer.finish(&Snapshot {
        head,
        history,
        prefix,
    })
}

pub(crate) async fn load_snapshot(
    owner: Reader<'_>,
    payload: EncodedPayload,
) -> Result<SessionSnapshot> {
    restore_snapshot(owner, payload.decode()?).await
}

pub(crate) async fn restore_snapshot(
    owner: Reader<'_>,
    saved: Snapshot,
) -> Result<SessionSnapshot> {
    load_snapshot_with_keys(owner, saved)
        .await
        .map(|(snapshot, _)| snapshot)
}

pub(crate) async fn load_snapshot_with_keys(
    owner: Reader<'_>,
    saved: Snapshot,
) -> Result<(SessionSnapshot, HashSet<String>)> {
    let mut keys = HashSet::new();
    let history = load_sequence(owner, saved.history, &mut keys).await?;
    let prefix = match saved.prefix {
        Some(value) => Some(load_sequence(owner, value, &mut keys).await?),
        None => None,
    };
    Ok((saved.head.with_context(history, prefix), keys))
}
