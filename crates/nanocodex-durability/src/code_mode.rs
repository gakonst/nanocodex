//! Native cell journal uses the authoritative model owner's fenced step actor.
use crate::{
    BeginStep, DocumentForkPolicy, DocumentWrite, DurableSession, Error, ReplaySafety, StepStatus,
    session::DurableOwner,
};
use nanocodex_oai_tools::code_mode::{CodeJournalAdmission, CodeModeJournal};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

const STORE_KEY: &str = "nanocodex.code-mode.store";
#[derive(Clone)]
struct Scope {
    operation: String,
    step: String,
}

pub(crate) struct DurableCodeJournal {
    owner: Arc<DurableOwner>,
    state: DurableSession,
    scopes: Mutex<HashMap<String, Scope>>,
    active: Mutex<HashMap<String, Scope>>,
}
impl DurableCodeJournal {
    pub(crate) fn new(owner: Arc<DurableOwner>, state: DurableSession) -> Self {
        Self {
            owner,
            state,
            scopes: Mutex::default(),
            active: Mutex::default(),
        }
    }
    pub(crate) fn bind(&self, operation: &str, step: &str, input: &str) -> crate::Result<()> {
        let value: Value = serde_json::from_str(input).map_err(Error::InvalidPayload)?;
        if value.get("name").and_then(Value::as_str) != Some("exec") {
            return Ok(());
        }
        let scope_key = json!([operation, step]).to_string();
        let mut scopes = self
            .scopes
            .lock()
            .map_err(|_| Error::InvalidState("Code Mode scope lock poisoned".into()))?;
        // Keep only live call scopes; the durable steps own historical identity.
        let active = self
            .active
            .lock()
            .map_err(|_| Error::InvalidState("Code Mode active lock poisoned".into()))?;
        if active.contains_key(&scope_key) {
            return Err(Error::InvalidState(
                "Code Mode call identity is still active; execution outcome unknown".into(),
            ));
        }
        if scopes.len() >= 64 && !scopes.contains_key(&scope_key) {
            return Err(Error::InvalidState(
                "Code Mode admission queue exceeds 64 cells".into(),
            ));
        }
        scopes.insert(
            scope_key,
            Scope {
                operation: operation.into(),
                step: format!("code-cell:{step}"),
            },
        );
        Ok(())
    }
    fn scope(&self, call: &str, active: bool) -> Result<Scope, String> {
        let map = if active { &self.active } else { &self.scopes };
        map.lock()
            .map_err(|_| "Code Mode scope lock poisoned")?
            .get(call)
            .cloned()
            .ok_or_else(|| "Code Mode operation scope missing; execution outcome unknown".into())
    }
}
#[async_trait::async_trait]
impl CodeModeJournal for DurableCodeJournal {
    async fn admit_cell(
        &self,
        _session_id: &str,
        call_id: &str,
        source: &str,
    ) -> Result<CodeJournalAdmission, String> {
        let scope = self
            .scopes
            .lock()
            .map_err(|_| "Code Mode scope lock poisoned")?
            .remove(call_id)
            .ok_or("Code Mode operation scope missing; execution outcome unknown")?;
        let admission = self
            .owner
            .begin_step(
                scope.operation.clone(),
                scope.step.clone(),
                "code_cell".into(),
                &json!({"session_id":self.state.state_id(),"source":source}),
                ReplaySafety::Unsafe,
            )
            .await
            .map_err(|e| e.to_string())?;
        match admission {
            BeginStep::OutcomeUnknown => {
                // The original unsafe cell is never executed again. Settle each
                // unfinished nested intent as unknown before retaining the cell
                // receipt, so the enclosing operation may advance. A crash while
                // reconciling repeats only missing settlements, never dispatch.
                let state = self.state.state().await.map_err(|e| e.to_string())?;
                let operation = state
                    .operation(&scope.operation)
                    .ok_or("Code Mode operation missing during recovery")?;
                let prefix = format!("{}/effect:", scope.step);
                let message = "Code Mode cell has an unfinished durable attempt; execution outcome unknown. External effects will not be redispatched.";
                let mut nested = Vec::new();
                for (step_id, step) in &operation.steps {
                    let Some(effect_id) = step_id.strip_prefix(&prefix) else {
                        continue;
                    };
                    if step.kind != "code_effect" {
                        continue;
                    }
                    let receipt = match &step.status {
                        StepStatus::Completed(output) => self
                            .owner
                            .load_payload(output.clone())
                            .await
                            .map_err(|e| e.to_string())?
                            .decode::<Value>()
                            .map_err(|e| e.to_string())?,
                        StepStatus::EffectPending => {
                            let input: Value = self
                                .owner
                                .load_payload(step.input.clone())
                                .await
                                .map_err(|e| e.to_string())?
                                .decode()
                                .map_err(|e| e.to_string())?;
                            let receipt = json!({
                                "call_id": effect_id, "name": input["name"], "input": input["input"],
                                "output": message, "structured_result": {
                                    "error": message, "code": "CODE_MODE_CALL_INTERRUPTED", "outcome": "unknown"
                                }, "success": false, "started_after_ns": 0, "duration_ns": 0, "metadata": null,
                            });
                            self.owner
                                .complete_step(scope.operation.clone(), step_id.clone(), &receipt)
                                .await
                                .map_err(|e| e.to_string())?;
                            receipt
                        }
                    };
                    nested.push(receipt);
                }
                let receipt = json!({
                    "cell": null, "output": format!("Script failed\nOutput:\n{message}"),
                    "success": false, "nested_calls": nested, "notifications": [],
                });
                self.owner
                    .complete_code_cell(scope.operation, scope.step, &receipt, vec![])
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(CodeJournalAdmission::Replay(receipt))
            }
            BeginStep::Replay(output) => Ok(CodeJournalAdmission::Replay(
                output.decode().map_err(|e| e.to_string())?,
            )),
            BeginStep::Execute => {
                let document = self
                    .state
                    .document(STORE_KEY)
                    .await
                    .map_err(|e| e.to_string())?;
                let (stored, version) = match document {
                    Some(doc) => (
                        serde_json::from_value(doc.value)
                            .map_err(|e| format!("Code Mode store corrupt: {e}"))?,
                        doc.version,
                    ),
                    None => (HashMap::new(), 0),
                };
                self.active
                    .lock()
                    .map_err(|_| "Code Mode active lock poisoned")?
                    .insert(call_id.into(), scope);
                Ok(CodeJournalAdmission::Execute { stored, version })
            }
        }
    }
    async fn begin_effect(
        &self,
        call_id: &str,
        effect_id: &str,
        name: &str,
        input: &Value,
    ) -> Result<(), String> {
        let scope = self.scope(call_id, true)?;
        match self
            .owner
            .begin_step(
                scope.operation,
                format!("{}/effect:{effect_id}", scope.step),
                "code_effect".into(),
                &json!({"name":name,"input":input}),
                ReplaySafety::Unsafe,
            )
            .await
            .map_err(|e| e.to_string())?
        {
            BeginStep::Execute => Ok(()),
            _ => Err("Code Mode nested effect already admitted; execution outcome unknown".into()),
        }
    }
    async fn complete_effect(
        &self,
        call_id: &str,
        effect_id: &str,
        receipt: &Value,
    ) -> Result<(), String> {
        let scope = self.scope(call_id, true)?;
        self.owner
            .complete_step(
                scope.operation,
                format!("{}/effect:{effect_id}", scope.step),
                receipt,
            )
            .await
            .map_err(|e| e.to_string())
    }
    async fn complete_cell(
        &self,
        call_id: &str,
        expected_version: u64,
        stored: Option<HashMap<String, Value>>,
        receipt: &Value,
    ) -> Result<(), String> {
        let scope = self.scope(call_id, true)?;
        let writes = match stored {
            Some(stored) => vec![DocumentWrite {
                key: STORE_KEY.into(),
                expected_version,
                value: serde_json::to_value(stored).map_err(|e| e.to_string())?,
                fork: DocumentForkPolicy::AsOf,
            }],
            None => vec![],
        };
        self.owner
            .complete_code_cell(scope.operation, scope.step, receipt, writes)
            .await
            .map_err(|e| e.to_string())?;
        self.active
            .lock()
            .map_err(|_| "Code Mode active lock poisoned")?
            .remove(call_id);
        Ok(())
    }
}
