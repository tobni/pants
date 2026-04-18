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
//!
//! The adapter's argument types drive lifting. Pick the representation the body actually
//! needs:
//! - `Value` — the engine's `Arc<Py<PyAny>>` handle. Default for untyped inputs, engine
//!   internals (`Key`, graph inputs, `unsafe_call` plumbing).
//! - `Value<FrozenPyClass>` — typed variant for a `#[pyclass(frozen)]` input. PyO3 extracts
//!   it at the adapter boundary (one cheap type check); the body uses `Value::get()` to
//!   reach `&T` without a GIL attach. Keeps the same Arc-optimized clone/drop as the
//!   untyped `Value`.

use std::future::Future;

use crate::externs::PyGeneratorResponseNativeCall;
use crate::nodes::NodeResult;
use crate::python::Value;

/// Adapter for a native rule taking zero Python-side arguments.
pub fn native_call0<F, Fut>(body: F) -> PyGeneratorResponseNativeCall
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = NodeResult<Value>> + Send + 'static,
{
    PyGeneratorResponseNativeCall::new(body())
}

/// Adapter for a native rule taking one Python-side argument.
///
/// `I` is whatever PyO3 extracted at the adapter boundary: `Value` for the untyped passthrough,
/// or `Py<T>` for a frozen pyclass — the harness just forwards it to the body.
pub fn native_call<I, F, Fut>(input: I, body: F) -> PyGeneratorResponseNativeCall
where
    I: Send + 'static,
    F: FnOnce(I) -> Fut + Send + 'static,
    Fut: Future<Output = NodeResult<Value>> + Send + 'static,
{
    PyGeneratorResponseNativeCall::new(body(input))
}

/// Adapter for a native rule taking two Python-side arguments.
pub fn native_call2<I1, I2, F, Fut>(a: I1, b: I2, body: F) -> PyGeneratorResponseNativeCall
where
    I1: Send + 'static,
    I2: Send + 'static,
    F: FnOnce(I1, I2) -> Fut + Send + 'static,
    Fut: Future<Output = NodeResult<Value>> + Send + 'static,
{
    PyGeneratorResponseNativeCall::new(body(a, b))
}
