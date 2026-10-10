use nanocodex::SessionCheckpoint as RustSessionCheckpoint;
use pyo3::{
    PyResult,
    exceptions::PyValueError,
    prelude::{pyclass, pymethods},
};

use crate::error::runtime_error;

/// Portable, versioned session boundary used to resume an agent.
#[pyclass(frozen, module = "nanocodex._native")]
pub(crate) struct SessionCheckpoint {
    inner: RustSessionCheckpoint,
}

#[pymethods]
impl SessionCheckpoint {
    /// Decode and validate a checkpoint previously returned by `to_json()`.
    #[staticmethod]
    fn from_json(encoded: &str) -> PyResult<Self> {
        RustSessionCheckpoint::from_json(encoded)
            .map(Self::new)
            .map_err(|error| PyValueError::new_err(error.to_string()))
    }

    /// Serialize this complete unredacted session boundary.
    fn to_json(&self) -> PyResult<String> {
        self.inner.to_json().map_err(runtime_error)
    }

    /// Identity of the checkpointed session; resuming keeps it.
    #[getter]
    fn session_id(&self) -> &str {
        self.inner.session_id()
    }

    /// Harness family that produced this checkpoint, such as `"codex"`.
    #[getter]
    const fn family(&self) -> &'static str {
        self.inner.family().as_str()
    }

    /// Identity of the turn that committed this boundary, when known.
    #[getter]
    fn turn_id(&self) -> Option<&str> {
        self.inner.turn_id()
    }

    /// Whether this checkpoint carries a committed conversation.
    #[getter]
    const fn has_conversation(&self) -> bool {
        self.inner.has_conversation()
    }

    fn __repr__(&self) -> String {
        format!(
            "SessionCheckpoint(session_id={:?}, family={:?}, has_conversation={})",
            self.inner.session_id(),
            self.inner.family().as_str(),
            if self.inner.has_conversation() {
                "True"
            } else {
                "False"
            }
        )
    }
}

impl SessionCheckpoint {
    pub(crate) const fn new(inner: RustSessionCheckpoint) -> Self {
        Self { inner }
    }

    pub(crate) const fn inner(&self) -> &RustSessionCheckpoint {
        &self.inner
    }
}
