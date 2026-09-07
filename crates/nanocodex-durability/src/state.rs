use std::{collections::BTreeMap, sync::Arc};

use crate::{Error, Result};
use serde::{Serialize, de::DeserializeOwned};

const STATE_FORMAT: u8 = 2;

/// A typed value erased only for storage in a heterogeneous state.
///
/// The wrapper preserves the original JSON representation. Consumers recover
/// concrete Rust types with [`Self::decode`]; hosts treat the containing state
/// as opaque bytes.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(transparent)]
pub struct EncodedPayload(Arc<str>);

impl<'de> serde::Deserialize<'de> for EncodedPayload {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct PayloadVisitor;
        impl serde::de::Visitor<'_> for PayloadVisitor {
            type Value = EncodedPayload;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("retained JSON text")
            }

            fn visit_str<E: serde::de::Error>(
                self,
                value: &str,
            ) -> std::result::Result<Self::Value, E> {
                // A streaming JSON decoder already owns its unescape buffer.
                // Copy straight into the shared allocation instead of first
                // materializing another full String/Box<str> for Arc's visitor.
                Ok(EncodedPayload(Arc::from(value)))
            }
        }
        deserializer.deserialize_str(PayloadVisitor)
    }
}

impl EncodedPayload {
    pub(crate) fn encode<T: Serialize + ?Sized>(value: &T) -> Result<Self> {
        serde_json::to_string(value)
            .map(|json| Self(Arc::from(json)))
            .map_err(Error::InvalidPayload)
    }

    /// Decodes this payload into its expected concrete type.
    pub fn decode<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_str(&self.0).map_err(Error::InvalidPayload)
    }

    /// Returns the exact retained JSON text.
    #[must_use]
    pub fn json(&self) -> &str {
        &self.0
    }
}

impl PartialEq for EncodedPayload {
    fn eq(&self, other: &Self) -> bool {
        self.json() == other.json()
    }
}

impl Eq for EncodedPayload {}

/// One Rust-owned durable state entry.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Transition {
    /// A host-visible operation was durably accepted.
    OperationAccepted {
        /// Caller-provided idempotency identity.
        operation_id: String,
        /// Opaque typed input encoded by the Rust consumer.
        input: EncodedPayload,
    },
    /// An external step began.
    StepStarted {
        /// Accepted operation identity.
        operation_id: String,
        /// Stable step identity within the operation.
        step_id: String,
        /// Semantic step kind used for diagnostics.
        kind: String,
        /// Opaque typed step input.
        input: EncodedPayload,
    },
    /// An external step completed with a replayable output.
    StepCompleted {
        /// Accepted operation identity.
        operation_id: String,
        /// Stable step identity within the operation.
        step_id: String,
        /// Opaque typed output returned during replay.
        output: EncodedPayload,
    },
    /// Live steering input was accepted for an active operation.
    SteerAccepted {
        /// Accepted operation identity.
        operation_id: String,
        /// Stable one-based FIFO position within the operation.
        steer_index: u32,
        /// Model call that was current when the steering input was accepted.
        accepted_after_model_call_index: u32,
        /// Exact typed steering prompt.
        input: EncodedPayload,
    },
    /// Accepted steering input was bound to its consuming model boundary.
    SteerBound {
        /// Accepted operation identity.
        operation_id: String,
        /// Stable one-based FIFO position within the operation.
        steer_index: u32,
        /// Model-call ordinal before which the steer is applied.
        model_call_index: u32,
    },
    /// An operation completed and advanced the durable session checkpoint.
    OperationCompleted {
        /// Accepted operation identity.
        operation_id: String,
        /// Opaque resumable agent checkpoint.
        checkpoint: EncodedPayload,
        /// Opaque completed result returned to duplicate submissions.
        output: EncodedPayload,
    },
    /// An operation failed and advanced the durable session checkpoint.
    OperationFailed {
        /// Accepted operation identity.
        operation_id: String,
        /// Opaque resumable agent checkpoint.
        checkpoint: EncodedPayload,
        /// Stable terminal failure detail.
        error: String,
    },
    /// An operation was explicitly cancelled.
    OperationCancelled {
        /// Accepted operation identity.
        operation_id: String,
        /// Safe interrupted checkpoint for an active operation. A queued
        /// cancellation has no new model boundary.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checkpoint: Option<EncodedPayload>,
    },
    /// A model-only boundary, such as explicit standalone compaction, advanced
    /// the resumable session without terminalizing an operation.
    CheckpointCommitted {
        /// Opaque resumable agent checkpoint.
        checkpoint: EncodedPayload,
    },
}

/// Reduced status of one operation.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationStatus {
    /// Accepted work may be attempted or resumed.
    Pending,
    /// Work completed with an opaque result and checkpoint.
    Completed {
        /// Resumable checkpoint committed atomically with the result.
        checkpoint: EncodedPayload,
        /// Result returned to duplicate submissions.
        output: EncodedPayload,
    },
    /// Work failed with a resumable checkpoint and retained diagnostic.
    Failed {
        /// Resumable checkpoint committed atomically with the failure.
        checkpoint: EncodedPayload,
        /// Failure returned to duplicate submissions.
        error: String,
    },
    /// Work was explicitly cancelled, optionally after advancing the safe
    /// interrupted checkpoint.
    Cancelled {
        /// Safe checkpoint committed by active cancellation.
        checkpoint: Option<EncodedPayload>,
    },
}

impl OperationStatus {
    /// Returns whether this operation cannot execute again.
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed { .. } | Self::Failed { .. } | Self::Cancelled { .. }
        )
    }
}

/// Reduced status of one step.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    /// The step started but has no committed output yet.
    EffectPending,
    /// The external effect's exact output settled durably.
    Completed(EncodedPayload),
}

/// Reduced durable step state.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct StepState {
    /// Semantic kind recorded by the caller.
    pub kind: String,
    /// Original opaque step input.
    pub input: EncodedPayload,
    /// Current reduced status.
    pub status: StepStatus,
    /// Number of committed starts for this step.
    pub attempts: u32,
}

/// One live steering input retained for deterministic operation recovery.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct SteerState {
    /// Exact typed steering prompt.
    pub input: EncodedPayload,
    /// Model call that was current when the steering input was accepted.
    pub accepted_after_model_call_index: u32,
    /// Model-call ordinal before which the steer is applied, once known.
    pub model_call_index: Option<u32>,
}

/// Reduced durable operation state.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct OperationState {
    /// Original opaque operation input.
    pub input: EncodedPayload,
    /// Current operation status.
    pub status: OperationStatus,
    /// Ordered durable steps by identity.
    pub steps: BTreeMap<String, StepState>,
    /// Live steering inputs in their accepted FIFO order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steers: Vec<SteerState>,
    pub(crate) accepted_order: u64,
}

/// Complete state reduced from an complete retained state.
#[derive(Clone, Debug, Default)]
pub struct DurableState {
    revision: u64,
    operations: BTreeMap<String, OperationState>,
    latest_checkpoint: Option<(u64, EncodedPayload)>,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DurableCheckpoint {
    format: u8,
    operations: BTreeMap<String, OperationState>,
    latest_checkpoint: Option<EncodedPayload>,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetainedCheckpoint {
    pub(crate) nanocodex_durable_state: DurableCheckpoint,
}

#[derive(serde::Serialize)]
struct DurableCheckpointRef<'a> {
    format: u8,
    operations: &'a BTreeMap<String, OperationState>,
    latest_checkpoint: Option<&'a EncodedPayload>,
}

#[derive(serde::Serialize)]
struct RetainedCheckpointRef<'a> {
    nanocodex_durable_state: DurableCheckpointRef<'a>,
}

impl DurableState {
    /// Current optimistic store revision.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Operations keyed by caller-provided identity.
    #[must_use]
    pub const fn operations(&self) -> &BTreeMap<String, OperationState> {
        &self.operations
    }

    /// Looks up one operation.
    #[must_use]
    pub fn operation(&self, operation_id: &str) -> Option<&OperationState> {
        self.operations.get(operation_id)
    }

    /// Returns accepted non-terminal operations in submission order.
    #[must_use]
    pub fn pending_operations(&self) -> Vec<(&str, &OperationState)> {
        let mut operations = self
            .operations
            .iter()
            .filter(|(_, operation)| !operation.status.is_terminal())
            .map(|(id, operation)| (id.as_str(), operation))
            .collect::<Vec<_>>();
        operations.sort_by_key(|(_, operation)| operation.accepted_order);
        operations
    }

    pub(crate) fn first_pending_operation(&self) -> Option<(&str, &OperationState)> {
        self.first_pending_operation_where(|_| true)
    }

    pub(crate) fn first_pending_operation_where(
        &self,
        mut predicate: impl FnMut(&str) -> bool,
    ) -> Option<(&str, &OperationState)> {
        self.operations
            .iter()
            .filter(|(id, operation)| !operation.status.is_terminal() && predicate(id.as_str()))
            .min_by_key(|(_, operation)| operation.accepted_order)
            .map(|(id, operation)| (id.as_str(), operation))
    }

    /// Returns the latest terminal checkpoint in operation order.
    #[must_use]
    pub fn latest_checkpoint(&self) -> Option<&EncodedPayload> {
        self.latest_checkpoint
            .as_ref()
            .map(|(_, checkpoint)| checkpoint)
    }

    pub(crate) fn checkpoint_payload(&self) -> Result<String> {
        crate::encoding::encode(&RetainedCheckpointRef {
            nanocodex_durable_state: DurableCheckpointRef {
                format: STATE_FORMAT,
                operations: &self.operations,
                latest_checkpoint: self.latest_checkpoint(),
            },
        })
        .map_err(Error::InvalidPayload)
    }

    pub(crate) fn retain_terminal_receipts(&mut self, limit: usize) -> bool {
        let before = self.operations.len();
        Self::retain_terminal_operations(&mut self.operations, limit);
        let mut changed = self.operations.len() != before;
        for operation in self
            .operations
            .values_mut()
            .filter(|operation| operation.status.is_terminal())
        {
            // Terminal replay uses only input, result, and checkpoint. Keeping
            // every intermediate full-history model request multiplies memory
            // and write volume across long conversations.
            changed |= !operation.steps.is_empty() || !operation.steers.is_empty();
            operation.steps.clear();
            operation.steers.clear();
        }
        changed
    }

    fn retain_terminal_operations(operations: &mut BTreeMap<String, OperationState>, limit: usize) {
        let mut terminal_orders = operations
            .values()
            .filter(|operation| operation.status.is_terminal())
            .map(|operation| operation.accepted_order)
            .collect::<Vec<_>>();
        terminal_orders.sort_unstable_by(|left, right| right.cmp(left));
        terminal_orders.truncate(limit);
        let retained = terminal_orders
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        operations.retain(|_, operation| {
            !operation.status.is_terminal() || retained.contains(&operation.accepted_order)
        });
    }

    pub(crate) fn from_checkpoint(revision: u64, checkpoint: DurableCheckpoint) -> Result<Self> {
        if revision == 0 {
            return Err(Error::InvalidState(
                "a compacted state checkpoint must have a positive revision".to_owned(),
            ));
        }
        if checkpoint.format != STATE_FORMAT {
            return Err(Error::InvalidState(format!(
                "unsupported state format {}",
                checkpoint.format
            )));
        }
        let mut accepted_orders = std::collections::BTreeSet::new();
        for (operation_id, operation) in &checkpoint.operations {
            ensure_nonempty(operation_id, "operation ID")?;
            if operation.accepted_order == 0
                || operation.accepted_order > revision
                || !accepted_orders.insert(operation.accepted_order)
            {
                return Err(Error::InvalidState(format!(
                    "operation `{operation_id}` has an invalid compacted acceptance order"
                )));
            }
            for (step_id, step) in &operation.steps {
                ensure_nonempty(step_id, "step ID")?;
                ensure_nonempty(&step.kind, "step kind")?;
                if step.attempts == 0 {
                    return Err(Error::InvalidState(format!(
                        "step `{step_id}` in operation `{operation_id}` has no committed start"
                    )));
                }
            }
            let mut previous_model_call_index = None;
            let mut saw_unbound_steer = false;
            for (offset, steer) in operation.steers.iter().enumerate() {
                if steer.accepted_after_model_call_index == 0
                    || steer.model_call_index.is_some_and(|model_call_index| {
                        model_call_index <= steer.accepted_after_model_call_index
                    })
                {
                    return Err(Error::InvalidState(format!(
                        "steer {} in operation `{operation_id}` has an invalid model boundary",
                        offset + 1
                    )));
                }
                match steer.model_call_index {
                    Some(current) => {
                        if saw_unbound_steer {
                            return Err(Error::InvalidState(format!(
                                "steer {} in operation `{operation_id}` was bound after an unbound steer",
                                offset + 1
                            )));
                        }
                        if previous_model_call_index.is_some_and(|previous| current < previous) {
                            return Err(Error::InvalidState(format!(
                                "steer {} in operation `{operation_id}` moved before an earlier steer",
                                offset + 1
                            )));
                        }
                        previous_model_call_index = Some(current);
                    }
                    None => saw_unbound_steer = true,
                }
            }
            if matches!(operation.status, OperationStatus::Completed { .. }) {
                ensure_completed_steers_consumed(operation_id, operation)?;
            }
            if matches!(
                &operation.status,
                OperationStatus::Cancelled { checkpoint: None }
            ) && (!operation.steps.is_empty() || !operation.steers.is_empty())
            {
                return Err(Error::InvalidState(format!(
                    "started operation `{operation_id}` was cancelled without a checkpoint"
                )));
            }
        }
        // Live transitions share the latest checkpoint with their terminal
        // receipt. Deserialization loses that Arc sharing; restore it before
        // constructing the agent so a cold reopen does not retain a second
        // full conversation. Standalone checkpoints can differ and stay intact.
        let latest_checkpoint = checkpoint.latest_checkpoint.map(|latest| {
            let shared = checkpoint.operations.values().rev().find_map(|operation| {
                let candidate = match &operation.status {
                    OperationStatus::Completed { checkpoint, .. }
                    | OperationStatus::Failed { checkpoint, .. }
                    | OperationStatus::Cancelled {
                        checkpoint: Some(checkpoint),
                    } => checkpoint,
                    _ => return None,
                };
                (candidate == &latest).then(|| candidate.clone())
            });
            (revision, shared.unwrap_or(latest))
        });
        let state = Self {
            revision,
            operations: checkpoint.operations,
            latest_checkpoint,
        };
        for (operation_id, operation) in &state.operations {
            if matches!(
                &operation.status,
                OperationStatus::Completed { .. }
                    | OperationStatus::Failed { .. }
                    | OperationStatus::Cancelled {
                        checkpoint: Some(_)
                    }
            ) {
                state.ensure_prior_operations_terminal(operation_id)?;
            }
        }
        Ok(state)
    }

    pub(crate) fn validate_transition(&self, revision: u64, entry: &Transition) -> Result<()> {
        let expected_revision = self.revision.checked_add(1).ok_or_else(|| {
            Error::InvalidState("state revision exceeded the u64 range".to_owned())
        })?;
        if revision != expected_revision {
            return Err(Error::InvalidState(format!(
                "expected revision {}, found {revision}",
                expected_revision
            )));
        }
        self.validate(entry)
    }

    pub(crate) fn apply_transition(&mut self, revision: u64, entry: Transition) -> Result<()> {
        self.validate_transition(revision, &entry)?;
        self.apply(revision, entry)?;
        self.revision = revision;
        Ok(())
    }

    pub(crate) fn advance_revision(&mut self, revision: u64) -> Result<()> {
        let expected_revision = self.revision.checked_add(1).ok_or_else(|| {
            Error::InvalidState("state revision exceeded the u64 range".to_owned())
        })?;
        if revision != expected_revision {
            return Err(Error::InvalidState(format!(
                "expected revision {}, found {revision}",
                expected_revision
            )));
        }
        self.revision = revision;
        Ok(())
    }

    fn validate(&self, entry: &Transition) -> Result<()> {
        if let Some(operation_id) = entry.operation_id() {
            ensure_nonempty(operation_id, "operation ID")?;
        }
        match entry {
            Transition::OperationAccepted { operation_id, .. } => {
                if self.operations.contains_key(operation_id) {
                    return Err(Error::InvalidState(format!(
                        "operation `{operation_id}` was accepted more than once"
                    )));
                }
            }
            Transition::StepStarted {
                operation_id,
                step_id,
                kind,
                input,
            } => {
                ensure_nonempty(step_id, "step ID")?;
                ensure_nonempty(kind, "step kind")?;
                self.ensure_prior_operations_terminal(operation_id)?;
                let operation = self.pending_operation(operation_id)?;
                if let Some(step) = operation.steps.get(step_id) {
                    if step.kind != *kind || step.input != *input {
                        return Err(Error::InvalidState(format!(
                            "step `{step_id}` in operation `{operation_id}` changed definition"
                        )));
                    }
                    if matches!(step.status, StepStatus::Completed(_)) {
                        return Err(Error::InvalidState(format!(
                            "settled step `{step_id}` in operation `{operation_id}` restarted"
                        )));
                    }
                    if step.attempts == u32::MAX {
                        return Err(Error::InvalidState(format!(
                            "step `{step_id}` in operation `{operation_id}` exceeded the attempt counter range"
                        )));
                    }
                }
            }
            Transition::StepCompleted {
                operation_id,
                step_id,
                output: _,
            } => {
                ensure_nonempty(step_id, "step ID")?;
                self.ensure_prior_operations_terminal(operation_id)?;
                let operation = self.pending_operation(operation_id)?;
                let step = operation.steps.get(step_id).ok_or_else(|| {
                    Error::InvalidState(format!(
                        "step `{step_id}` in operation `{operation_id}` completed before start"
                    ))
                })?;
                match &step.status {
                    StepStatus::EffectPending => {}
                    StepStatus::Completed(_) => {
                        return Err(Error::InvalidState(format!(
                            "step `{step_id}` in operation `{operation_id}` completed more than once"
                        )));
                    }
                }
            }
            Transition::SteerAccepted {
                operation_id,
                steer_index,
                accepted_after_model_call_index,
                input: _,
            } => {
                self.ensure_prior_operations_terminal(operation_id)?;
                if *accepted_after_model_call_index == 0 {
                    return Err(Error::InvalidState(format!(
                        "steer {steer_index} in operation `{operation_id}` has an invalid acceptance boundary"
                    )));
                }
                let operation = self.pending_operation(operation_id)?;
                let expected = u32::try_from(operation.steers.len())
                    .ok()
                    .and_then(|length| length.checked_add(1))
                    .ok_or_else(|| {
                        Error::InvalidState(format!(
                            "operation `{operation_id}` exceeded the steer counter range"
                        ))
                    })?;
                if *steer_index != expected {
                    return Err(Error::InvalidState(format!(
                        "operation `{operation_id}` expected steer {expected}, found {steer_index}"
                    )));
                }
            }
            Transition::SteerBound {
                operation_id,
                steer_index,
                model_call_index,
            } => {
                self.ensure_prior_operations_terminal(operation_id)?;
                if *model_call_index == 0 {
                    return Err(Error::InvalidState(format!(
                        "steer {steer_index} in operation `{operation_id}` has an invalid model boundary"
                    )));
                }
                let operation = self.pending_operation(operation_id)?;
                let steer = steer_index
                    .checked_sub(1)
                    .and_then(|index| usize::try_from(index).ok())
                    .and_then(|index| operation.steers.get(index))
                    .ok_or_else(|| {
                        Error::InvalidState(format!(
                            "steer {steer_index} in operation `{operation_id}` was bound before acceptance"
                        ))
                    })?;
                if *model_call_index <= steer.accepted_after_model_call_index {
                    return Err(Error::InvalidState(format!(
                        "steer {steer_index} in operation `{operation_id}` cannot bind to model call {model_call_index} after acceptance at {}",
                        steer.accepted_after_model_call_index
                    )));
                }
                if steer.model_call_index.is_some() {
                    return Err(Error::InvalidState(format!(
                        "steer {steer_index} in operation `{operation_id}` was bound more than once"
                    )));
                }
                if *steer_index > 1 {
                    let previous_index = usize::try_from(*steer_index - 2).map_err(|_| {
                        Error::InvalidState(format!(
                            "steer {steer_index} in operation `{operation_id}` has an invalid index"
                        ))
                    })?;
                    let previous = &operation.steers[previous_index];
                    let Some(previous_model_call_index) = previous.model_call_index else {
                        return Err(Error::InvalidState(format!(
                            "steer {steer_index} in operation `{operation_id}` was bound before an earlier steer"
                        )));
                    };
                    if *model_call_index < previous_model_call_index {
                        return Err(Error::InvalidState(format!(
                            "steer {steer_index} in operation `{operation_id}` moved before an earlier steer"
                        )));
                    }
                }
            }
            Transition::OperationCompleted { operation_id, .. } => {
                self.ensure_prior_operations_terminal(operation_id)?;
                let operation = self.pending_operation(operation_id)?;
                if operation
                    .steps
                    .values()
                    .any(|step| !matches!(step.status, StepStatus::Completed(_)))
                {
                    return Err(Error::InvalidState(format!(
                        "operation `{operation_id}` completed with an unfinished step"
                    )));
                }
                ensure_completed_steers_consumed(operation_id, operation)?;
            }
            Transition::OperationFailed { operation_id, .. } => {
                self.ensure_prior_operations_terminal(operation_id)?;
                self.pending_operation(operation_id)?;
            }
            Transition::OperationCancelled {
                operation_id,
                checkpoint,
            } => {
                let operation = self.pending_operation(operation_id)?;
                if checkpoint.is_some() {
                    self.ensure_prior_operations_terminal(operation_id)?;
                } else if !operation.steps.is_empty() || !operation.steers.is_empty() {
                    return Err(Error::InvalidState(format!(
                        "started operation `{operation_id}` was cancelled without a checkpoint"
                    )));
                }
            }
            Transition::CheckpointCommitted { .. } => {
                if let Some((pending_id, _)) = self.first_pending_operation() {
                    return Err(Error::InvalidState(format!(
                        "standalone checkpoint effect crossed pending operation `{pending_id}`"
                    )));
                }
            }
        }
        Ok(())
    }

    fn apply(&mut self, revision: u64, entry: Transition) -> Result<()> {
        match entry {
            Transition::OperationAccepted {
                operation_id,
                input,
            } => {
                self.operations.insert(
                    operation_id,
                    OperationState {
                        input,
                        status: OperationStatus::Pending,
                        steps: BTreeMap::new(),
                        steers: Vec::new(),
                        accepted_order: revision,
                    },
                );
            }
            Transition::StepStarted {
                operation_id,
                step_id,
                kind,
                input,
            } => {
                let operation = self.pending_operation_mut(&operation_id)?;
                if let Some(step) = operation.steps.get_mut(&step_id) {
                    step.attempts = step.attempts.checked_add(1).ok_or_else(|| {
                        Error::InvalidState(format!(
                            "step `{step_id}` in operation `{operation_id}` exceeded the attempt counter range"
                        ))
                    })?;
                } else {
                    operation.steps.insert(
                        step_id,
                        StepState {
                            kind,
                            input,
                            status: StepStatus::EffectPending,
                            attempts: 1,
                        },
                    );
                }
            }
            Transition::StepCompleted {
                operation_id,
                step_id,
                output,
            } => {
                let operation = self.pending_operation_mut(&operation_id)?;
                let step = operation.steps.get_mut(&step_id).ok_or_else(|| {
                    Error::InvalidState(format!(
                        "step `{step_id}` in operation `{operation_id}` completed before start"
                    ))
                })?;
                step.status = StepStatus::Completed(output);
            }
            Transition::SteerAccepted {
                operation_id,
                steer_index: _,
                accepted_after_model_call_index,
                input,
            } => {
                self.pending_operation_mut(&operation_id)?
                    .steers
                    .push(SteerState {
                        input,
                        accepted_after_model_call_index,
                        model_call_index: None,
                    });
            }
            Transition::SteerBound {
                operation_id,
                steer_index,
                model_call_index,
            } => {
                let operation = self.pending_operation_mut(&operation_id)?;
                let index = usize::try_from(steer_index - 1).map_err(|_| {
                    Error::InvalidState(format!(
                        "steer {steer_index} in operation `{operation_id}` has an invalid index"
                    ))
                })?;
                operation.steers[index].model_call_index = Some(model_call_index);
            }
            Transition::OperationCompleted {
                operation_id,
                checkpoint,
                output,
            } => {
                let operation = self.pending_operation_mut(&operation_id)?;
                operation.status = OperationStatus::Completed {
                    checkpoint: checkpoint.clone(),
                    output,
                };
                self.latest_checkpoint = Some((revision, checkpoint));
            }
            Transition::OperationFailed {
                operation_id,
                checkpoint,
                error,
            } => {
                let operation = self.pending_operation_mut(&operation_id)?;
                operation.status = OperationStatus::Failed {
                    checkpoint: checkpoint.clone(),
                    error,
                };
                self.latest_checkpoint = Some((revision, checkpoint));
            }
            Transition::OperationCancelled {
                operation_id,
                checkpoint,
            } => {
                let operation = self.pending_operation_mut(&operation_id)?;
                operation.status = OperationStatus::Cancelled {
                    checkpoint: checkpoint.clone(),
                };
                if let Some(checkpoint) = checkpoint {
                    self.latest_checkpoint = Some((revision, checkpoint));
                }
            }
            Transition::CheckpointCommitted { checkpoint } => {
                self.latest_checkpoint = Some((revision, checkpoint));
            }
        }
        Ok(())
    }

    fn pending_operation_mut(&mut self, operation_id: &str) -> Result<&mut OperationState> {
        let operation = self.operations.get_mut(operation_id).ok_or_else(|| {
            Error::InvalidState(format!("operation `{operation_id}` was not accepted"))
        })?;
        if operation.status.is_terminal() {
            return Err(Error::InvalidState(format!(
                "terminal operation `{operation_id}` was changed"
            )));
        }
        Ok(operation)
    }

    fn pending_operation(&self, operation_id: &str) -> Result<&OperationState> {
        let operation = self.operations.get(operation_id).ok_or_else(|| {
            Error::InvalidState(format!("operation `{operation_id}` was not accepted"))
        })?;
        if operation.status.is_terminal() {
            return Err(Error::InvalidState(format!(
                "terminal operation `{operation_id}` was changed"
            )));
        }
        Ok(operation)
    }

    fn ensure_prior_operations_terminal(&self, operation_id: &str) -> Result<()> {
        let operation = self.operations.get(operation_id).ok_or_else(|| {
            Error::InvalidState(format!("operation `{operation_id}` was not accepted"))
        })?;
        if let Some((pending_id, _)) = self.operations.iter().find(|(id, candidate)| {
            candidate.accepted_order < operation.accepted_order
                && !candidate.status.is_terminal()
                && id.as_str() != operation_id
        }) {
            return Err(Error::InvalidState(format!(
                "operation `{operation_id}` completed before `{pending_id}`"
            )));
        }
        Ok(())
    }
}

fn ensure_completed_steers_consumed(operation_id: &str, operation: &OperationState) -> Result<()> {
    for (offset, steer) in operation.steers.iter().enumerate() {
        let steer_index = offset + 1;
        let model_call_index = steer.model_call_index.ok_or_else(|| {
            Error::InvalidState(format!(
                "operation `{operation_id}` completed with unbound steer {steer_index}"
            ))
        })?;
        let step_id = format!("model-{model_call_index}");
        let consumed = operation.steps.get(&step_id).is_some_and(|step| {
            step.kind == "model_call" && matches!(step.status, StepStatus::Completed(_))
        });
        if !consumed {
            return Err(Error::InvalidState(format!(
                "operation `{operation_id}` completed before steer {steer_index} was consumed by `{step_id}`"
            )));
        }
    }
    Ok(())
}

impl Transition {
    fn operation_id(&self) -> Option<&str> {
        match self {
            Self::OperationAccepted { operation_id, .. }
            | Self::StepStarted { operation_id, .. }
            | Self::StepCompleted { operation_id, .. }
            | Self::SteerAccepted { operation_id, .. }
            | Self::SteerBound { operation_id, .. }
            | Self::OperationCompleted { operation_id, .. }
            | Self::OperationFailed { operation_id, .. }
            | Self::OperationCancelled { operation_id, .. } => Some(operation_id),
            Self::CheckpointCommitted { .. } => None,
        }
    }
}

fn ensure_nonempty(value: &str, name: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::InvalidState(format!("{name} must not be empty")));
    }
    Ok(())
}
