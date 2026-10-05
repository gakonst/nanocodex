//! Versioned session-owned documents, explicitly committed with operation results.

use crate::{EncodedPayload, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Value selected when a session branches at a retained document boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentForkPolicy {
    /// Restore the value supplied when this document was created.
    Initial,
    /// Copy the source's current value, even at an older boundary.
    Current,
    /// Copy the value at the selected boundary; omit documents not yet created.
    AsOf,
    /// Refuse forks while this document exists.
    Block,
}

/// A document's value and optimistic revision within its session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionDocument {
    /// One-based document revision, independent from the storage revision.
    pub version: u64,
    /// Immutable creation value used by the initial fork policy.
    pub initial: Value,
    /// Current JSON value.
    pub value: Value,
    /// Immutable fork policy chosen on creation.
    pub fork: DocumentForkPolicy,
}

/// One conditional mutation. Omission from a transaction preserves a document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentWrite {
    /// Session-local key.
    pub key: String,
    /// Zero creates; updates require the exact current document revision.
    pub expected_version: u64,
    /// New JSON value. JSON null is a value, not deletion.
    pub value: Value,
    /// Creation policy; updates must repeat the original policy.
    pub fork: DocumentForkPolicy,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Documents {
    pub(crate) current: BTreeMap<String, SessionDocument>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) staged: Option<StagedDocuments>,
    // Legacy indexes are read on migration; new boundaries exist only until staged.
    // The execution head never serializes an unbounded boundary index.
    #[serde(default, skip_serializing)]
    pub(crate) boundaries: BTreeMap<String, EncodedPayload>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StagedDocuments {
    pub(crate) operation_id: String,
    pub(crate) writes: Vec<DocumentWrite>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Boundary {
    pub(crate) checkpoint: EncodedPayload,
    pub(crate) documents: BTreeMap<String, SessionDocument>,
}

/// An explicit branch seed. It carries data only, never credentials or schedules.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentFork {
    /// Source document boundary (a completed operation ID).
    pub boundary: String,
    /// Policy-selected documents. Versions restart at one in the destination.
    pub documents: BTreeMap<String, SessionDocument>,
}

impl Documents {
    pub(crate) fn commit(
        &mut self,
        id: &str,
        checkpoint: EncodedPayload,
        writes: Vec<DocumentWrite>,
    ) -> Result<()> {
        if self.boundaries.contains_key(id) {
            return Err(Error::InvalidState(format!(
                "document boundary `{id}` already exists"
            )));
        }
        self.write(writes)?;
        let boundary = EncodedPayload::encode(&Boundary {
            checkpoint,
            documents: self.current.clone(),
        })?;
        self.boundaries.insert(id.to_owned(), boundary);
        Ok(())
    }

    pub(crate) fn write(&mut self, writes: Vec<DocumentWrite>) -> Result<()> {
        let mut seen = std::collections::BTreeSet::new();
        for write in writes {
            if write.key.trim().is_empty()
                || write.key.len() > 256
                || !seen.insert(write.key.clone())
            {
                return Err(Error::InvalidState(
                    "document keys must be nonempty, unique and at most 256 bytes".into(),
                ));
            }
            let previous = self.current.get(&write.key);
            if previous.map_or(0, |doc| doc.version) != write.expected_version
                || previous.is_some_and(|doc| doc.fork != write.fork)
            {
                return Err(Error::InvalidState(format!(
                    "document `{}` version or policy conflict",
                    write.key
                )));
            }
            let version = write
                .expected_version
                .checked_add(1)
                .ok_or_else(|| Error::InvalidState("document version overflow".into()))?;
            let initial = previous.map_or_else(|| write.value.clone(), |doc| doc.initial.clone());
            self.current.insert(
                write.key,
                SessionDocument {
                    version,
                    initial,
                    value: write.value,
                    fork: write.fork,
                },
            );
        }
        self.validate()
    }

    pub(crate) fn stage(&mut self, operation_id: String, writes: Vec<DocumentWrite>) -> Result<()> {
        let mut staged = match &self.staged {
            Some(staged) if staged.operation_id != operation_id => {
                return Err(Error::InvalidState(
                    "another operation has staged documents".into(),
                ));
            }
            Some(staged) => staged.clone(),
            None => StagedDocuments {
                operation_id,
                writes: Vec::new(),
            },
        };
        for write in writes {
            if staged.writes.iter().any(|existing| existing == &write) {
                continue;
            }
            staged.writes.push(write);
        }
        if staged.writes.len() > 64
            || serde_json::to_vec(&staged)
                .map_err(Error::InvalidPayload)?
                .len()
                > 65_536
        {
            return Err(Error::InvalidState(
                "staged documents exceed 64 writes or 65536 encoded bytes".into(),
            ));
        }
        let mut preview = self.clone();
        preview.write(staged.writes.clone())?;
        self.staged = Some(staged);
        Ok(())
    }

    pub(crate) fn take_staged(&mut self, operation_id: &str) -> Vec<DocumentWrite> {
        if self
            .staged
            .as_ref()
            .is_some_and(|staged| staged.operation_id == operation_id)
        {
            self.staged.take().expect("checked staged documents").writes
        } else {
            Vec::new()
        }
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.current.len() > 64
            || serde_json::to_vec(&self.current)
                .map_err(Error::InvalidPayload)?
                .len()
                > 65_536
        {
            return Err(Error::InvalidState(
                "session documents exceed 64 documents or 65536 encoded bytes".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn fork(&self, id: &str, boundary: Boundary) -> Result<DocumentFork> {
        let mut documents = BTreeMap::new();
        for (key, doc) in &self.current {
            let mut value = match doc.fork {
                DocumentForkPolicy::Block => {
                    return Err(Error::InvalidState(format!(
                        "document `{key}` blocks forking"
                    )));
                }
                DocumentForkPolicy::Initial => SessionDocument {
                    value: doc.initial.clone(),
                    ..doc.clone()
                },
                DocumentForkPolicy::Current => doc.clone(),
                DocumentForkPolicy::AsOf => match boundary.documents.get(key) {
                    Some(value) => value.clone(),
                    None => continue,
                },
            };
            value.version = 1;
            documents.insert(key.clone(), value);
        }
        Ok(DocumentFork {
            boundary: id.to_owned(),
            documents,
        })
    }
}

// Namespaced, deterministic lookup identity; content records remain content addressed.
pub(crate) fn boundary_key(id: &str) -> String {
    format!("document-boundary/{}", crate::state::record_key(id))
}
