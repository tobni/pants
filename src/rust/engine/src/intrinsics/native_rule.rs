// Copyright 2026 Pants project contributors (see CONTRIBUTORS.md).
// Licensed under the Apache License, Version 2.0 (see LICENSE).

//! Harness for native rules.
//!
//! A native rule is a Rust async fn that the engine can invoke the same way a Python `@rule`
//! is invoked. Rules never take an explicit `Context` argument: the task-local `TASK_CONTEXT`
//! is fetched via `task_get_context()` inside the body as needed. Callers compose rules by
//! directly awaiting them.
//!
//! The Python boundary is a thin `#[pyfunction]` adapter that delegates to one of the
//! `native_callN` helpers. `N` is the number of Python-side arguments the rule takes.

#![allow(dead_code)]

use std::future::Future;

use pyo3::Python;

use crate::externs::PyGeneratorResponseNativeCall;
use crate::nodes::NodeResult;
use crate::python::Value;

/// How a native rule lifts a Python value into its Rust input type.
///
/// The default `Value` passthrough impl lets intrinsics opt out of typed lifting and do their
/// own Python-side extraction; typed inputs replace that pattern once a rule wants to compose
/// with other native rules without a round-trip through Python.
pub trait NativeRuleInput: Sized + Send + 'static {
    fn lift(py: Python<'_>, value: &Value) -> NodeResult<Self>;
}

/// How a native rule stores its Rust output back as a Python value.
pub trait NativeRuleOutput: Sized + Send + 'static {
    fn store(self, py: Python<'_>) -> NodeResult<Value>;
}

impl NativeRuleInput for Value {
    fn lift(_py: Python<'_>, value: &Value) -> NodeResult<Self> {
        Ok(value.clone())
    }
}

impl NativeRuleOutput for Value {
    fn store(self, _py: Python<'_>) -> NodeResult<Value> {
        Ok(self)
    }
}

/// Adapter for a native rule taking zero Python-side arguments.
pub fn native_call0<O, F, Fut>(body: F) -> PyGeneratorResponseNativeCall
where
    O: NativeRuleOutput,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = NodeResult<O>> + Send + 'static,
{
    PyGeneratorResponseNativeCall::new(async move {
        let out = body().await?;
        Python::attach(|py| O::store(out, py))
    })
}

/// Adapter for a native rule taking one Python-side argument.
pub fn native_call<I, O, F, Fut>(value: Value, body: F) -> PyGeneratorResponseNativeCall
where
    I: NativeRuleInput,
    O: NativeRuleOutput,
    F: FnOnce(I) -> Fut + Send + 'static,
    Fut: Future<Output = NodeResult<O>> + Send + 'static,
{
    PyGeneratorResponseNativeCall::new(async move {
        let input = Python::attach(|py| I::lift(py, &value))?;
        let out = body(input).await?;
        Python::attach(|py| O::store(out, py))
    })
}

/// Adapter for a native rule taking two Python-side arguments.
pub fn native_call2<I1, I2, O, F, Fut>(
    a: Value,
    b: Value,
    body: F,
) -> PyGeneratorResponseNativeCall
where
    I1: NativeRuleInput,
    I2: NativeRuleInput,
    O: NativeRuleOutput,
    F: FnOnce(I1, I2) -> Fut + Send + 'static,
    Fut: Future<Output = NodeResult<O>> + Send + 'static,
{
    PyGeneratorResponseNativeCall::new(async move {
        let (i1, i2) = Python::attach(|py| -> NodeResult<(I1, I2)> {
            Ok((I1::lift(py, &a)?, I2::lift(py, &b)?))
        })?;
        let out = body(i1, i2).await?;
        Python::attach(|py| O::store(out, py))
    })
}
