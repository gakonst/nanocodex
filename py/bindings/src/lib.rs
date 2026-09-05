mod agent;
mod error;
mod events;
mod runtime;
mod snapshot;
mod turn;
mod usage;

use pyo3::{
    Bound, PyResult,
    prelude::{PyModule, pymodule},
    types::PyModuleMethods,
};

use self::{
    agent::Nanocodex,
    events::{AgentEvent, AgentEvents},
    snapshot::SessionSnapshot,
    turn::{Turn, TurnResult},
};

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    module.add_class::<Nanocodex>()?;
    module.add_class::<Turn>()?;
    module.add_class::<TurnResult>()?;
    module.add_class::<AgentEvent>()?;
    module.add_class::<AgentEvents>()?;
    module.add_class::<SessionSnapshot>()?;
    Ok(())
}
