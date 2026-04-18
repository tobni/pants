// Copyright 2026 Pants project contributors (see CONTRIBUTORS.md).
// Licensed under the Apache License, Version 2.0 (see LICENSE).

//! Rule-graph boundary types for Rust-native rules.
//!
//! A `RuleType` is any Rust type that can cross the rule-graph boundary: it knows its Python
//! counterpart's `TypeId`, can be `lift`ed out of a `Value`, and can be `store`d back into one.
//!
//! `implicitly<I, O>(input)` is the Rust mirror of Python's `await target_rule(**implicitly(...))`:
//! it asks the rule-graph solver for the (unique) path from `I` to `O`, dispatches it through
//! `ctx.get(Task{...})`, and returns the typed product. Full `Task`-level caching applies.

use std::future::IntoFuture;
use std::marker::PhantomData;

use fs::{DirectoryDigest, PathGlobs};
use futures::future::BoxFuture;
use pyo3::PyTypeInfo;
use pyo3::Python;
use pyo3::prelude::PyAnyMethods;
use pyo3::{Bound, Py, PyAny};
use rule_graph::DependencyKey;

use crate::externs::INTERNS;
use crate::externs::fs::{PyDigest, PyPathGlobs, PySnapshot};
use crate::nodes::{NodeResult, Snapshot, lift_directory_digest, select, task_get_context};
use crate::python::{Failure, Params, TypeId, Value, throw};

/// A type that can cross the rule-graph boundary as a Python `Value`.
pub trait RuleType: Sized + Send + 'static {
    /// The `TypeId` of the Python type that this Rust type round-trips to/from.
    fn python_type_id(py: Python<'_>) -> TypeId;

    /// Convert a `Value` carrying the Python counterpart back into this Rust type.
    fn lift(py: Python<'_>, value: &Value) -> NodeResult<Self>;

    /// Convert this Rust type into a `Value` of the Python counterpart.
    fn store(self, py: Python<'_>) -> NodeResult<Value>;
}

/// A pending rule-graph call from `I` to `O`. Awaits to `NodeResult<O>` via the graph, and
/// — because it implements [`RuleArg<O>`] — drops directly into another native rule's arg
/// position:
///
/// ```ignore
/// // Bare use (let-bound):
/// let digest: DirectoryDigest = implicitly(globs).await?;
///
/// // Call-by-name via `RuleArg`, mirrors Python's `await target(**implicitly(...))`:
/// let snapshot = digest_to_snapshot(implicitly(globs)).await?;
/// ```
pub struct Implicitly<I: RuleType, O: RuleType> {
    input: I,
    _marker: PhantomData<fn() -> O>,
}

/// Construct a pending rule-graph call. `I` is the input's type, `O` the product — both
/// usually inferred from the consuming position (`let x: O = ...` or an enclosing
/// `impl RuleArg<O>` parameter).
pub fn implicitly<I: RuleType, O: RuleType>(input: I) -> Implicitly<I, O> {
    Implicitly {
        input,
        _marker: PhantomData,
    }
}

impl<I: RuleType, O: RuleType> IntoFuture for Implicitly<I, O> {
    type Output = NodeResult<O>;
    type IntoFuture = BoxFuture<'static, NodeResult<O>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(resolve_implicit::<I, O>(self.input))
    }
}

/// Graph-dispatches a call from `I` to `O`. The rule graph must contain a registered path —
/// every native rule's `install_into` emits a `QueryRule(O, [I])` for its own shape, and the
/// `#[native_rule]` macro AST-scans bodies for `Implicitly<I, O>` occurrences and registers
/// the chain-spanning pairs.
async fn resolve_implicit<I: RuleType, O: RuleType>(input: I) -> NodeResult<O> {
    let ctx = task_get_context();

    let (input_key, input_type, output_type) = Python::attach(|py| -> NodeResult<_> {
        let input_value = input.store(py)?;
        let input_type = I::python_type_id(py);
        let output_type = O::python_type_id(py);
        let py_any = input_value.bind(py).clone().unbind();
        let input_key = INTERNS
            .key_insert(py, py_any)
            .map_err(|e| throw(format!("implicitly: key interning failed: {e}")))?;
        Ok((input_key, input_type, output_type))
    })?;

    let params = Params::new([input_key]).map_err(throw)?;
    let (_root, edges) = ctx
        .core
        .rule_graph
        .find_root([input_type], output_type)
        .map_err(|e| throw(format!("implicitly({input_type} → {output_type}): {e}")))?;
    let entry = edges
        .entry_for(&DependencyKey::new(output_type))
        .ok_or_else(|| {
            throw(format!(
                "implicitly({input_type} → {output_type}): no edge for output"
            ))
        })?;

    let result_value = select(ctx, None, 0, params, entry).await?;
    Python::attach(|py| O::lift(py, &result_value))
}

/// A value usable as a native-rule argument. Lets the generated public fn accept either a
/// direct `T` or an `Implicitly<I, T>` — the latter resolves via the rule graph first so the
/// same callsite supports both `rule(value)` and `rule(implicitly(other_value))`.
///
/// `resolve` uses RPITIT (return-position impl trait in trait) so identity resolution stays
/// allocation-free; only `IntoFuture` for bare `.await` bounces through `BoxFuture`.
pub trait RuleArg<O: RuleType>: Send + 'static {
    fn resolve(self) -> impl std::future::Future<Output = NodeResult<O>> + Send;
}

impl<T: RuleType> RuleArg<T> for T {
    #[allow(clippy::manual_async_fn)] // explicit `+ Send` keeps impls usable in tokio tasks.
    fn resolve(self) -> impl std::future::Future<Output = NodeResult<T>> + Send {
        async move { Ok(self) }
    }
}

impl<I: RuleType, O: RuleType> RuleArg<O> for Implicitly<I, O> {
    fn resolve(self) -> impl std::future::Future<Output = NodeResult<O>> + Send {
        resolve_implicit::<I, O>(self.input)
    }
}

impl RuleType for PathGlobs {
    fn python_type_id(py: Python<'_>) -> TypeId {
        TypeId::new(&PyPathGlobs::type_object(py).as_borrowed())
    }

    fn lift(py: Python<'_>, value: &Value) -> NodeResult<Self> {
        let bound: Bound<'_, PyPathGlobs> = value
            .bind(py)
            .extract()
            .map_err(|e| throw(format!("Expected PathGlobs: {e}")))?;
        Ok(PathGlobs::clone(&*bound.borrow()))
    }

    fn store(self, py: Python<'_>) -> NodeResult<Value> {
        let obj = Py::new(py, PyPathGlobs::from_path_globs(self))
            .map_err(|e| Failure::from(format!("{e}")))?;
        Ok(Value::from(obj.bind(py)))
    }
}

impl RuleType for DirectoryDigest {
    fn python_type_id(py: Python<'_>) -> TypeId {
        TypeId::new(&PyDigest::type_object(py).as_borrowed())
    }

    fn lift(py: Python<'_>, value: &Value) -> NodeResult<Self> {
        let bound: &Bound<'_, PyAny> = value.bind(py);
        lift_directory_digest(bound).map_err(throw)
    }

    fn store(self, py: Python<'_>) -> NodeResult<Value> {
        Snapshot::store_directory_digest(py, self).map_err(Failure::from)
    }
}

impl RuleType for store::Snapshot {
    fn python_type_id(py: Python<'_>) -> TypeId {
        TypeId::new(&PySnapshot::type_object(py).as_borrowed())
    }

    fn lift(py: Python<'_>, value: &Value) -> NodeResult<Self> {
        let bound: Bound<'_, PySnapshot> = value
            .bind(py)
            .extract()
            .map_err(|e| throw(format!("Expected Snapshot: {e}")))?;
        Ok(bound.borrow().0.clone())
    }

    fn store(self, py: Python<'_>) -> NodeResult<Value> {
        Snapshot::store_snapshot(py, self).map_err(Failure::from)
    }
}

/// Marker trait opt-in for frozen `#[pyclass]` types that want the default `RuleType` impl.
/// Add `impl FrozenPyClassRuleType for MyType {}` once the pyclass is `frozen + Clone` and any
/// caller can pass/receive it directly through `implicitly` and `#[native_rule]`.
///
/// (Using a sealed opt-in marker instead of a plain blanket keeps the existing
/// non-pyclass impls for `PathGlobs`/`DirectoryDigest`/`store::Snapshot` unambiguous and gives
/// rule authors a single declaration point per type.)
pub trait FrozenPyClassRuleType:
    pyo3::PyClass<Frozen = pyo3::pyclass::boolean_struct::True>
    + Clone
    + Send
    + Sync
    + Into<pyo3::PyClassInitializer<Self>>
    + 'static
{
}

impl<T> RuleType for T
where
    T: FrozenPyClassRuleType,
{
    fn python_type_id(py: Python<'_>) -> TypeId {
        TypeId::new(&T::type_object(py).as_borrowed())
    }

    fn lift(py: Python<'_>, value: &Value) -> NodeResult<Self> {
        let bound: Bound<'_, T> = value
            .bind(py)
            .extract()
            .map_err(|e| throw(format!("{e}")))?;
        Ok(bound.borrow().clone())
    }

    fn store(self, py: Python<'_>) -> NodeResult<Value> {
        let obj = Py::new(py, self).map_err(|e| Failure::from(format!("{e}")))?;
        Ok(Value::from(obj.bind(py)))
    }
}
