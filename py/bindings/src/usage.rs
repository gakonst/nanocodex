use nanocodex::TurnUsage;
use pyo3::{
    Bound, Py, PyResult, Python,
    types::{PyDict, PyDictMethods},
};

pub(crate) fn usage_dict(py: Python<'_>, usage: &TurnUsage) -> PyResult<Py<PyDict>> {
    let output = PyDict::new(py);
    output.set_item("input_tokens", usage.input_tokens())?;
    output.set_item("cached_input_tokens", usage.cached_input_tokens())?;
    output.set_item("cache_write_input_tokens", usage.cache_write_input_tokens())?;
    output.set_item("output_tokens", usage.output_tokens())?;
    output.set_item("reasoning_output_tokens", usage.reasoning_output_tokens())?;
    output.set_item("total_tokens", usage.total_tokens())?;
    if let Some(cost) = usage.estimated_cost() {
        output.set_item("estimated_cost", estimated_cost_dict(py, cost)?)?;
    } else {
        output.set_item("estimated_cost", py.None())?;
    }
    output.set_item("cost_status", usage.cost_status().as_str())?;
    Ok(output.unbind())
}

fn estimated_cost_dict<'py>(
    py: Python<'py>,
    cost: &nanocodex::EstimatedUsdCost,
) -> PyResult<Bound<'py, PyDict>> {
    let output = PyDict::new(py);
    output.set_item("usd", cost.amount().decimal())?;
    output.set_item("input_usd", cost.input().decimal())?;
    output.set_item("cached_input_usd", cost.cached_input().decimal())?;
    output.set_item("cache_write_input_usd", cost.cache_write_input().decimal())?;
    output.set_item("output_usd", cost.output().decimal())?;
    output.set_item("service_tier", cost.service_tier().as_str())?;
    Ok(output)
}
