use pyo3::{PyErr, exceptions::PyRuntimeError};

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn runtime_error(error: impl ToString) -> PyErr {
    PyRuntimeError::new_err(error.to_string())
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn lock_error<T>(error: std::sync::PoisonError<T>) -> PyErr {
    PyRuntimeError::new_err(format!("binding state lock was poisoned: {error}"))
}
