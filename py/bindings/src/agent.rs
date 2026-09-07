use std::sync::{Arc, Mutex};

use nanocodex::{
    Model, Nanocodex as RustNanocodex, OpenAi, ReasoningMode, Thinking,
    agent::session::SessionId,
    oai::auth::{OpenAiAuth, load_chatgpt_auth},
};
use pyo3::{
    Py, PyResult, Python,
    exceptions::{PyRuntimeError, PyValueError},
    prelude::{pyclass, pymethods},
};
use tokio::runtime::Runtime;

use crate::{
    error::{lock_error, runtime_error},
    events::AgentEvents,
    runtime::{runtime, shared_http_client},
    snapshot::SessionSnapshot,
    turn::{Turn, TurnResult},
};

/// Cheap Python command handle for one owned native agent session.
#[pyclass(frozen, module = "nanocodex._native")]
pub(crate) struct Nanocodex {
    runtime: Arc<Runtime>,
    agent: Mutex<Option<RustNanocodex>>,
    session_id: String,
}

#[pymethods]
impl Nanocodex {
    #[new]
    #[pyo3(signature = (
        api_key = None,
        *,
        auth_file = None,
        model = "gpt-6-astra",
        thinking = None,
        reasoning_mode = "standard",
        fast_mode = false,
        workspace = None,
        instructions = None,
        session_id = None,
        prompt_cache_key = None,
        resume = None,
        websocket_url = None,
        api_base_url = None
    ))]
    #[allow(
        clippy::too_many_arguments,
        reason = "PyO3 exposes one flat keyword-only options surface"
    )]
    fn new(
        py: Python<'_>,
        api_key: Option<String>,
        auth_file: Option<String>,
        model: &str,
        thinking: Option<&str>,
        reasoning_mode: &str,
        fast_mode: bool,
        workspace: Option<String>,
        instructions: Option<String>,
        session_id: Option<String>,
        prompt_cache_key: Option<String>,
        resume: Option<Py<SessionSnapshot>>,
        websocket_url: Option<String>,
        api_base_url: Option<String>,
    ) -> PyResult<(Self, AgentEvents)> {
        let auth = parse_auth(api_key, auth_file)?;
        let model = parse_model(model)?;
        let thinking = thinking.map(parse_thinking).transpose()?;
        let reasoning_mode = parse_reasoning_mode(reasoning_mode)?;
        let session_id = session_id
            .map(|session_id| session_id.parse::<SessionId>())
            .transpose()
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        let resume = resume.map(|snapshot| snapshot.borrow(py).inner().clone());
        let mut openai = OpenAi::builder(auth)
            .model(model)
            .reasoning_mode(reasoning_mode)
            .fast_mode(fast_mode)
            .http_client(shared_http_client());
        if let Some(thinking) = thinking {
            openai = openai.thinking(thinking);
        }
        if let Some(websocket_url) = websocket_url {
            openai = openai.websocket_url(websocket_url);
        }
        if let Some(api_base_url) = api_base_url {
            openai = openai.api_base_url(api_base_url);
        }
        let openai = openai
            .build()
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        let runtime = runtime()?;
        let runtime_for_build = Arc::clone(&runtime);
        let (agent, events) = py
            .detach(move || {
                runtime_for_build.block_on(async move {
                    let mut builder = RustNanocodex::builder(openai);
                    if let Some(workspace) = workspace {
                        builder = builder.workspace(workspace);
                    }
                    if let Some(instructions) = instructions {
                        builder = builder.instructions(instructions);
                    }
                    if let Some(session_id) = session_id {
                        builder = builder.session_id(session_id);
                    }
                    if let Some(prompt_cache_key) = prompt_cache_key {
                        builder = builder.prompt_cache_key(prompt_cache_key);
                    }
                    if let Some(resume) = resume {
                        builder = builder.resume(resume);
                    }
                    builder.build()
                })
            })
            .map_err(runtime_error)?;
        Ok(wrap_agent(runtime, agent, events))
    }

    /// Stable UUIDv7 identity used by this agent's event stream.
    #[getter]
    fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Accept a prompt and immediately return its independently awaitable turn.
    fn prompt(&self, py: Python<'_>, prompt: String) -> PyResult<Turn> {
        let runtime = Arc::clone(&self.runtime);
        let agent = self.agent()?;
        let turn = py
            .detach(move || runtime.block_on(agent.prompt(prompt)))
            .map_err(runtime_error)?;
        Ok(Turn::new(Arc::clone(&self.runtime), turn))
    }

    /// Change the reasoning effort for subsequently accepted turns.
    fn set_thinking(&self, py: Python<'_>, thinking: &str) -> PyResult<()> {
        let thinking = parse_thinking(thinking)?;
        let runtime = Arc::clone(&self.runtime);
        let agent = self.agent()?;
        py.detach(move || runtime.block_on(agent.set_thinking(thinking)))
            .map_err(runtime_error)
    }

    /// Change the model before the first turn is accepted.
    fn set_model(&self, py: Python<'_>, model: &str) -> PyResult<()> {
        let model = parse_model(model)?;
        let runtime = Arc::clone(&self.runtime);
        let agent = self.agent()?;
        py.detach(move || runtime.block_on(agent.set_model(model)))
            .map_err(runtime_error)
    }

    /// Enable or disable priority processing for subsequently accepted turns.
    fn set_fast_mode(&self, py: Python<'_>, enabled: bool) -> PyResult<()> {
        let runtime = Arc::clone(&self.runtime);
        let agent = self.agent()?;
        py.detach(move || runtime.block_on(agent.set_fast_mode(enabled)))
            .map_err(runtime_error)
    }

    /// Compact retained history immediately without fabricating a user prompt.
    fn compact(&self, py: Python<'_>) -> PyResult<()> {
        let runtime = Arc::clone(&self.runtime);
        let agent = self.agent()?;
        py.detach(move || runtime.block_on(agent.compact()))
            .map_err(runtime_error)
    }

    /// Start a clean sibling agent with the same private configuration.
    fn spawn(&self, py: Python<'_>) -> PyResult<(Self, AgentEvents)> {
        let runtime = Arc::clone(&self.runtime);
        let agent = self.agent()?;
        let (child, events) = py
            .detach(move || runtime.block_on(agent.spawn()))
            .map_err(runtime_error)?;
        Ok(wrap_agent(Arc::clone(&self.runtime), child, events))
    }

    /// Fork from the latest safe model boundary.
    fn fork(&self, py: Python<'_>) -> PyResult<(Self, AgentEvents)> {
        let runtime = Arc::clone(&self.runtime);
        let agent = self.agent()?;
        let (child, events) = py
            .detach(move || runtime.block_on(agent.fork()))
            .map_err(runtime_error)?;
        Ok(wrap_agent(Arc::clone(&self.runtime), child, events))
    }

    /// Fork from the exact checkpoint retained by a completed historical turn.
    fn fork_from(&self, py: Python<'_>, completed: &TurnResult) -> PyResult<(Self, AgentEvents)> {
        let completed = completed.inner().clone();
        let runtime = Arc::clone(&self.runtime);
        let agent = self.agent()?;
        let (child, events) = py
            .detach(move || runtime.block_on(agent.fork_from(&completed)))
            .map_err(runtime_error)?;
        Ok(wrap_agent(Arc::clone(&self.runtime), child, events))
    }

    /// Gracefully stop the driver and join all resources owned by this agent.
    fn shutdown(&self, py: Python<'_>) -> PyResult<()> {
        let agent = self
            .agent
            .lock()
            .map_err(lock_error)?
            .take()
            .ok_or_else(agent_stopped)?;
        let runtime = Arc::clone(&self.runtime);
        py.detach(move || runtime.block_on(agent.shutdown()))
            .map_err(runtime_error)
    }

    fn __repr__(&self) -> String {
        format!("Nanocodex(session_id='{}')", self.session_id)
    }
}

impl Nanocodex {
    fn agent(&self) -> PyResult<RustNanocodex> {
        self.agent
            .lock()
            .map_err(lock_error)?
            .as_ref()
            .cloned()
            .ok_or_else(agent_stopped)
    }
}

fn wrap_agent(
    runtime: Arc<Runtime>,
    agent: RustNanocodex,
    events: nanocodex::AgentEvents,
) -> (Nanocodex, AgentEvents) {
    let session_id = agent.session_id().to_string();
    (
        Nanocodex {
            runtime: Arc::clone(&runtime),
            agent: Mutex::new(Some(agent)),
            session_id,
        },
        AgentEvents::new(runtime, events),
    )
}

fn parse_auth(api_key: Option<String>, auth_file: Option<String>) -> PyResult<OpenAiAuth> {
    match (api_key, auth_file) {
        (Some(api_key), None) => Ok(OpenAiAuth::api_key(api_key)),
        (None, Some(auth_file)) => load_chatgpt_auth(auth_file).map_err(runtime_error),
        (Some(_), Some(_)) => Err(PyValueError::new_err(
            "pass either api_key or auth_file, not both",
        )),
        (None, None) => Err(PyValueError::new_err("api_key or auth_file is required")),
    }
}

fn parse_thinking(value: &str) -> PyResult<Thinking> {
    value.parse().map_err(PyValueError::new_err)
}

fn parse_model(value: &str) -> PyResult<Model> {
    value.parse().map_err(PyValueError::new_err)
}

fn parse_reasoning_mode(value: &str) -> PyResult<ReasoningMode> {
    value.parse().map_err(PyValueError::new_err)
}

fn agent_stopped() -> pyo3::PyErr {
    PyRuntimeError::new_err("agent has been shut down")
}
