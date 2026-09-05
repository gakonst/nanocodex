use std::sync::{Arc, OnceLock};

use nanocodex::{AgentEvents as RustAgentEvents, agent::events::AgentEvent as RustAgentEvent};
use pyo3::{
    IntoPyObjectExt, Py, PyAny, PyResult, Python,
    prelude::{pyclass, pymethods},
    types::{PyDict, PyDictMethods, PyList},
};
use serde_json::{Value, value::RawValue};
use tokio::{runtime::Runtime, sync::Mutex};

use crate::error::runtime_error;

/// Independent ordered lifecycle-event receiver for one agent.
#[pyclass(frozen, module = "nanocodex._native")]
pub(crate) struct AgentEvents {
    runtime: Arc<Runtime>,
    request_id: String,
    events: Arc<Mutex<RustAgentEvents>>,
}

#[pymethods]
impl AgentEvents {
    /// Stable session/request identity shared by every event.
    #[getter]
    fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Block for and return one typed lifecycle event.
    fn recv(&self, py: Python<'_>) -> PyResult<Option<AgentEvent>> {
        let runtime = Arc::clone(&self.runtime);
        let events = Arc::clone(&self.events);
        let event =
            py.detach(move || runtime.block_on(async move { events.lock().await.recv().await }));
        event.map(AgentEvent::try_from).transpose()
    }

    /// Block for one event and return its exact JSON representation.
    ///
    /// `recv()` is the normal typed library path. This method is a convenience
    /// for applications that already own a JSONL boundary.
    fn recv_json(&self, py: Python<'_>) -> PyResult<Option<String>> {
        let runtime = Arc::clone(&self.runtime);
        let events = Arc::clone(&self.events);
        let event =
            py.detach(move || runtime.block_on(async move { events.lock().await.recv().await }));
        event
            .map(|event| serde_json::to_string(&event).map_err(runtime_error))
            .transpose()
    }

    fn __repr__(&self) -> String {
        format!("AgentEvents(request_id='{}')", self.request_id)
    }
}

/// One ordered typed lifecycle event emitted by an agent.
#[pyclass(frozen, module = "nanocodex._native")]
pub(crate) struct AgentEvent {
    protocol_version: u32,
    request_id: String,
    seq: u64,
    kind: String,
    payload: Arc<RawValue>,
    decoded_payload: OnceLock<Value>,
}

#[pymethods]
impl AgentEvent {
    /// Stable event protocol version.
    #[getter]
    const fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    /// Stable identity shared by this agent's event stream.
    #[getter]
    fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Monotonic sequence number within this event stream.
    #[getter]
    const fn seq(&self) -> u64 {
        self.seq
    }

    /// Stable event category such as `run.completed`.
    #[getter]
    fn kind(&self) -> &str {
        &self.kind
    }

    /// Complete event payload as native Python values.
    #[getter]
    fn payload(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let payload = if let Some(payload) = self.decoded_payload.get() {
            payload
        } else {
            let parsed = serde_json::from_str(self.payload.get()).map_err(runtime_error)?;
            let _ = self.decoded_payload.set(parsed);
            self.decoded_payload
                .get()
                .ok_or_else(|| runtime_error("agent event payload cache was not initialized"))?
        };
        json_to_python(py, payload)
    }

    /// Complete event payload encoded as exact compact JSON.
    #[getter]
    fn payload_json(&self) -> &str {
        self.payload.get()
    }

    fn __repr__(&self) -> String {
        format!(
            "AgentEvent(request_id='{}', seq={}, kind='{}')",
            self.request_id, self.seq, self.kind
        )
    }
}

impl TryFrom<RustAgentEvent> for AgentEvent {
    type Error = pyo3::PyErr;

    fn try_from(event: RustAgentEvent) -> Result<Self, Self::Error> {
        let kind = serde_json::to_value(event.kind)
            .map_err(runtime_error)?
            .as_str()
            .ok_or_else(|| runtime_error("agent event kind did not encode as a string"))?
            .to_owned();
        Ok(Self {
            protocol_version: event.protocol_version,
            request_id: event.request_id.to_string(),
            seq: event.seq,
            kind,
            payload: event.payload,
            decoded_payload: OnceLock::new(),
        })
    }
}

impl AgentEvents {
    pub(crate) fn new(runtime: Arc<Runtime>, events: RustAgentEvents) -> Self {
        let request_id = events.request_id().to_owned();
        Self {
            runtime,
            request_id,
            events: Arc::new(Mutex::new(events)),
        }
    }
}

fn json_to_python(py: Python<'_>, value: &Value) -> PyResult<Py<PyAny>> {
    match value {
        Value::Null => Ok(py.None()),
        Value::Bool(value) => value.into_py_any(py),
        Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                value.into_py_any(py)
            } else if let Some(value) = value.as_u64() {
                value.into_py_any(py)
            } else if let Some(value) = value.as_f64() {
                value.into_py_any(py)
            } else {
                Err(runtime_error(
                    "JSON number could not be represented in Python",
                ))
            }
        }
        Value::String(value) => value.into_py_any(py),
        Value::Array(values) => {
            let values = values
                .iter()
                .map(|value| json_to_python(py, value))
                .collect::<PyResult<Vec<_>>>()?;
            Ok(PyList::new(py, values)?.into_any().unbind())
        }
        Value::Object(values) => {
            let output = PyDict::new(py);
            for (name, value) in values {
                output.set_item(name, json_to_python(py, value)?)?;
            }
            Ok(output.into_any().unbind())
        }
    }
}
