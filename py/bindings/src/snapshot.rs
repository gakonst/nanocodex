use nanocodex::agent::session::SessionSnapshot as RustSessionSnapshot;
use pyo3::{
    PyResult,
    exceptions::PyValueError,
    prelude::{pyclass, pymethods},
};

use crate::error::runtime_error;

/// Serializable completed session boundary used to resume an agent.
#[pyclass(frozen, module = "nanocodex._native")]
pub(crate) struct SessionSnapshot {
    inner: RustSessionSnapshot,
}

#[pymethods]
impl SessionSnapshot {
    /// Decode a snapshot previously returned by `to_json()`.
    #[staticmethod]
    fn from_json(encoded: &str) -> PyResult<Self> {
        serde_json::from_str(encoded)
            .map(Self::new)
            .map_err(|error| PyValueError::new_err(error.to_string()))
    }

    /// Serialize this complete unredacted session boundary.
    fn to_json(&self) -> PyResult<String> {
        serde_json::to_string(&self.inner).map_err(runtime_error)
    }

    /// Snapshot format version understood by this release.
    #[getter]
    const fn version(&self) -> u32 {
        self.inner.version()
    }

    /// Absolute workspace retained by this session boundary.
    #[getter]
    fn workspace(&self) -> &str {
        self.inner.workspace()
    }

    fn __repr__(&self) -> String {
        format!(
            "SessionSnapshot(version={}, workspace={:?})",
            self.inner.version(),
            self.inner.workspace()
        )
    }
}

impl SessionSnapshot {
    pub(crate) const fn new(inner: RustSessionSnapshot) -> Self {
        Self { inner }
    }

    pub(crate) const fn inner(&self) -> &RustSessionSnapshot {
        &self.inner
    }
}
