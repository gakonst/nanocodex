use std::sync::{Arc, Mutex};

use nanocodex::{
    AgentEvents as RustAgentEvents, Nanocodex as RustNanocodex, OpenAiAuth, ReasoningMode,
    Thinking, load_chatgpt_auth,
};
use pyo3::{
    Bound, PyResult, Python,
    exceptions::{PyRuntimeError, PyValueError},
    prelude::{PyModule, pyclass, pymethods, pymodule},
    types::PyModuleMethods,
};
use tokio::runtime::Runtime;

#[pyclass(frozen, module = "nanocodex._native")]
struct Nanocodex {
    runtime: Arc<Runtime>,
    agent: RustNanocodex,
}

#[pymethods]
impl Nanocodex {
    #[new]
    #[pyo3(signature = (api_key = None, *, auth_file = None, thinking = "medium", reasoning_mode = "standard", workspace = None, instructions = None))]
    fn new(
        api_key: Option<String>,
        auth_file: Option<String>,
        thinking: &str,
        reasoning_mode: &str,
        workspace: Option<String>,
        instructions: Option<String>,
    ) -> PyResult<(Self, AgentEvents)> {
        let auth = match (api_key, auth_file) {
            (Some(api_key), None) => OpenAiAuth::api_key(api_key),
            (None, Some(auth_file)) => load_chatgpt_auth(auth_file).map_err(runtime_error)?,
            (Some(_), Some(_)) => {
                return Err(PyValueError::new_err(
                    "pass either api_key or auth_file, not both",
                ));
            }
            (None, None) => {
                return Err(PyValueError::new_err("api_key or auth_file is required"));
            }
        };
        let thinking = parse_thinking(thinking)?;
        let reasoning_mode = parse_reasoning_mode(reasoning_mode)?;
        let runtime = build_runtime()?;
        let (agent, events) = runtime
            .block_on(async move {
                let mut builder = RustNanocodex::builder(auth)
                    .reasoning_mode(reasoning_mode)
                    .thinking(thinking);
                if let Some(workspace) = workspace {
                    builder = builder.workspace(workspace);
                }
                if let Some(instructions) = instructions {
                    builder = builder.instructions(instructions);
                }
                builder.build()
            })
            .map_err(runtime_error)?;
        Ok((
            Self {
                runtime: Arc::clone(&runtime),
                agent,
            },
            AgentEvents {
                runtime,
                events: Arc::new(tokio::sync::Mutex::new(events)),
            },
        ))
    }

    /// Accept a prompt and immediately return its independently awaitable turn.
    fn prompt(&self, py: Python<'_>, prompt: String) -> PyResult<Turn> {
        let runtime = Arc::clone(&self.runtime);
        let agent = self.agent.clone();
        let turn = py
            .detach(move || runtime.block_on(agent.prompt(prompt)))
            .map_err(runtime_error)?;
        Ok(Turn {
            runtime: Arc::clone(&self.runtime),
            state: Mutex::new(TurnState::Pending(turn)),
        })
    }

    fn __repr__(&self) -> String {
        format!(
            "Nanocodex(runtime_references={})",
            Arc::strong_count(&self.runtime)
        )
    }
}

#[pyclass(module = "nanocodex._native")]
struct Turn {
    runtime: Arc<Runtime>,
    state: Mutex<TurnState>,
}

enum TurnState {
    Pending(nanocodex::Turn),
    Waiting,
    Completed(String),
    Failed(String),
}

#[pymethods]
impl Turn {
    /// Block until the turn completes and return its final assistant message.
    fn result(&self, py: Python<'_>) -> PyResult<String> {
        let turn = {
            let mut state = self.state.lock().map_err(lock_error)?;
            match &*state {
                TurnState::Completed(message) => return Ok(message.clone()),
                TurnState::Failed(error) => return Err(PyRuntimeError::new_err(error.clone())),
                TurnState::Waiting => {
                    return Err(PyRuntimeError::new_err(
                        "another thread is already waiting for this turn",
                    ));
                }
                TurnState::Pending(_) => {}
            }
            match std::mem::replace(&mut *state, TurnState::Waiting) {
                TurnState::Pending(turn) => turn,
                _ => unreachable!("pending state was checked before replacement"),
            }
        };

        let runtime = Arc::clone(&self.runtime);
        match py.detach(move || runtime.block_on(turn.result())) {
            Ok(result) => {
                let message = result.final_message;
                *self.state.lock().map_err(lock_error)? = TurnState::Completed(message.clone());
                Ok(message)
            }
            Err(error) => {
                let error = error.to_string();
                *self.state.lock().map_err(lock_error)? = TurnState::Failed(error.clone());
                Err(PyRuntimeError::new_err(error))
            }
        }
    }
}

#[pyclass(frozen, module = "nanocodex._native")]
struct AgentEvents {
    runtime: Arc<Runtime>,
    events: Arc<tokio::sync::Mutex<RustAgentEvents>>,
}

#[pymethods]
impl AgentEvents {
    /// Block for one event and return its exact JSON representation.
    fn recv_json(&self, py: Python<'_>) -> PyResult<Option<String>> {
        let runtime = Arc::clone(&self.runtime);
        let events = Arc::clone(&self.events);
        let event =
            py.detach(move || runtime.block_on(async move { events.lock().await.recv().await }));
        event
            .map(|event| serde_json::to_string(&event).map_err(runtime_error))
            .transpose()
    }
}

fn build_runtime() -> PyResult<Arc<Runtime>> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map(Arc::new)
        .map_err(runtime_error)
}

fn parse_thinking(value: &str) -> PyResult<Thinking> {
    value.parse().map_err(PyValueError::new_err)
}

fn parse_reasoning_mode(value: &str) -> PyResult<ReasoningMode> {
    value.parse().map_err(PyValueError::new_err)
}

#[allow(clippy::needless_pass_by_value)]
fn runtime_error(error: impl ToString) -> pyo3::PyErr {
    PyRuntimeError::new_err(error.to_string())
}

#[allow(clippy::needless_pass_by_value)]
fn lock_error<T>(error: std::sync::PoisonError<T>) -> pyo3::PyErr {
    PyRuntimeError::new_err(format!("binding state lock was poisoned: {error}"))
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<Nanocodex>()?;
    module.add_class::<Turn>()?;
    module.add_class::<AgentEvents>()?;
    Ok(())
}
