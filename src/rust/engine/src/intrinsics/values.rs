// Copyright 2021 Pants project contributors (see CONTRIBUTORS.md).
// Licensed under the Apache License, Version 2.0 (see LICENSE).

use pyo3::types::{PyModule, PyModuleMethods};
use pyo3::{Bound, PyResult, Python, wrap_pyfunction};

use crate::nodes::{NodeResult, RunId, SessionValues, task_get_context};
use crate::python::Value;

pub fn register(_py: Python, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(py_bindings::session_values, m)?)?;
    m.add_function(wrap_pyfunction!(py_bindings::run_id, m)?)?;

    Ok(())
}

pub async fn session_values() -> NodeResult<Value> {
    task_get_context().get(SessionValues).await
}

pub async fn run_id() -> NodeResult<Value> {
    task_get_context().get(RunId).await
}

mod py_bindings {
    use pyo3::pyfunction;

    use crate::externs::PyGeneratorResponseNativeCall;
    use crate::intrinsics::native_rule::native_call0;

    #[pyfunction]
    pub fn session_values() -> PyGeneratorResponseNativeCall {
        native_call0(super::session_values)
    }

    #[pyfunction]
    pub fn run_id() -> PyGeneratorResponseNativeCall {
        native_call0(super::run_id)
    }
}
