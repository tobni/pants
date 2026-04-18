// Copyright 2026 Pants project contributors (see CONTRIBUTORS.md).
// Licensed under the Apache License, Version 2.0 (see LICENSE).

//! Registry of Rust-native `@rule`-equivalents.
//!
//! A native rule is a Rust async fn that participates in the engine rule graph the same way a
//! Python `@rule` does: it has a unique `RuleId`, typed input/output, and is dispatched through
//! `ctx.get(Task{...})` so its result is cached at the Graph level exactly like a Python rule.
//!
//! Entries live in a single global registry keyed by `RuleId`. At engine bootstrap the scheduler
//! drains the registry and calls into `Tasks` to install a graph entry for each native rule. At
//! runtime, `Task::run_node` looks up the rule's `RuleId` in the registry and — if present —
//! executes the registered fn on the resolved deps instead of calling a Python callable.

use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

use futures::future::BoxFuture;
use log::Level;
use pyo3::Python;
use rule_graph::RuleId;

use crate::nodes::NodeResult;
use crate::python::{TypeId, Value};

/// A native rule body after dispatch. Receives the resolved dependency values in the same order
/// they were declared during registration; returns the rule's product value.
pub type NativeRuleFn = fn(Vec<Value>) -> BoxFuture<'static, NodeResult<Value>>;

/// Graph shape for a native rule. Produced at bootstrap time — the Python type objects a rule
/// references are only accessible once the interpreter is initialized, so `NativeRuleRegistration`
/// defers computing this until the scheduler is being created.
#[derive(Clone)]
pub struct NativeRuleSignature {
    /// Human-friendly rule name (used as the workunit label).
    pub name: String,
    /// The `TypeId` of the rule's output (Python product).
    pub product: TypeId,
    /// Ordered `(arg_name, TypeId)` pairs for the rule's declared inputs. Each entry becomes a
    /// `DependencyKey` in the rule graph — the engine resolves each before invoking the body.
    pub args: Vec<(String, TypeId)>,
    /// Parameter types the rule is not allowed to receive (inherited from `@rule`'s `masked`).
    pub masked_types: Vec<TypeId>,
    /// Optional user-facing description.
    pub desc: Option<String>,
    /// Workunit level.
    pub level: Level,
}

/// Details required to install a native rule into the engine's rule graph at bootstrap.
#[derive(Clone)]
pub struct NativeRuleRegistration {
    pub rule_id: RuleId,
    pub body: NativeRuleFn,
    /// Resolves the rule's signature using the Python interpreter. Called once during
    /// `scheduler_create` to install a graph entry for the rule.
    pub signature: fn(Python<'_>) -> NativeRuleSignature,
    /// Resolves the set of `QueryRule(product, params)` pairs that this rule's body reaches
    /// via `implicitly::<I, O>(...)`. The `#[native_rule]` macro derives these from an AST scan
    /// of the body. Empty for rules that don't call `implicitly`.
    pub queries: fn(Python<'_>) -> Vec<(TypeId, Vec<TypeId>)>,
}

static NATIVE_RULES: LazyLock<RwLock<HashMap<RuleId, NativeRuleRegistration>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Register a native rule. Panics if the `rule_id` is already registered, to surface duplicate
/// `#[native_rule]` attributes early.
pub fn register(registration: NativeRuleRegistration) {
    let mut guard = NATIVE_RULES.write().unwrap();
    if let Some(existing) = guard.get(&registration.rule_id) {
        panic!(
            "Duplicate native rule registration for {}: already registered as {}",
            registration.rule_id, existing.rule_id
        );
    }
    guard.insert(registration.rule_id.clone(), registration);
}

/// Look up a native rule body. Returns `None` for Python `@rule`s.
pub fn lookup(rule_id: &RuleId) -> Option<NativeRuleFn> {
    NATIVE_RULES.read().unwrap().get(rule_id).map(|r| r.body)
}

/// All currently-registered native rules. Used at bootstrap to install graph entries.
pub fn iter_registrations() -> Vec<NativeRuleRegistration> {
    NATIVE_RULES.read().unwrap().values().cloned().collect()
}

/// Install every registered native rule as a graph entry on the given `Tasks`, and emit a
/// `QueryRule`-equivalent so callers can reach the rule via `RuleGraph::find_root`. Must be
/// called after Python `@rule`s have been drained into `Tasks` and before `Core::new` builds
/// the rule graph from those tasks.
pub fn install_into(py: Python<'_>, tasks: &mut crate::tasks::Tasks) {
    for reg in iter_registrations() {
        let signature = (reg.signature)(py);
        let param_types: Vec<TypeId> = signature.args.iter().map(|(_, t)| *t).collect();
        tasks.task_begin_native(
            reg.rule_id.clone(),
            signature.product,
            signature.args,
            signature.masked_types,
            signature.name,
            signature.desc,
            signature.level,
        );
        tasks.task_end();
        // The rule's own `(input → output)` as a queryable root.
        tasks.query_add(signature.product, param_types);
        // Plus every `implicitly::<I, O>` pair the body reaches — AST-extracted by the macro.
        for (product, params) in (reg.queries)(py) {
            tasks.query_add(product, params);
        }
    }
}

/// Expose each registered native rule as a Python-callable `RuleCallTrampoline` on the given
/// module. Python callers can then `await native_engine.<name>(input)` to yield a `Call` that
/// the engine resolves through the graph (with full `Task`-level caching), instead of the
/// uncached `NativeCall` path that bare `#[pyfunction]` intrinsics use.
///
/// The attribute name on the module is the rule's short name (the `RuleId` stripped of its
/// `native::` prefix).
pub fn install_trampolines(
    py: Python<'_>,
    m: &pyo3::Bound<'_, pyo3::types::PyModule>,
) -> pyo3::PyResult<()> {
    use pyo3::prelude::PyAnyMethods;
    use pyo3::types::{PyModuleMethods, PyString};
    use pyo3::{Py, PyAny};

    use crate::externs::RuleCallTrampoline;

    for reg in iter_registrations() {
        let signature = (reg.signature)(py);
        let output_type: Py<pyo3::types::PyType> = signature.product.as_py_type(py).unbind();
        let rule_id_str = PyString::new(py, reg.rule_id.as_str());
        let rule_id_backed: pyo3::pybacked::PyBackedStr = rule_id_str.extract()?;
        let none_wrapped: Py<PyAny> = py.None();
        let none_rule: Py<PyAny> = py.None();
        let trampoline = Py::new(
            py,
            RuleCallTrampoline::new(rule_id_backed, output_type, none_wrapped, none_rule),
        )?;
        let short_name = signature
            .name
            .strip_prefix("native::")
            .unwrap_or(signature.name.as_str());
        m.add(short_name, trampoline)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt;

    fn echo_body(deps: Vec<Value>) -> BoxFuture<'static, NodeResult<Value>> {
        async move {
            assert_eq!(deps.len(), 1);
            Ok(deps.into_iter().next().unwrap())
        }
        .boxed()
    }

    fn echo_signature(_py: Python<'_>) -> NativeRuleSignature {
        unreachable!("test doesn't exercise bootstrap")
    }

    fn echo_queries(_py: Python<'_>) -> Vec<(TypeId, Vec<TypeId>)> {
        unreachable!("test doesn't exercise bootstrap")
    }

    #[test]
    fn register_and_lookup_roundtrip() {
        let id = RuleId::new("native_rules::test_echo");
        register(NativeRuleRegistration {
            rule_id: id.clone(),
            body: echo_body,
            signature: echo_signature,
            queries: echo_queries,
        });
        let looked_up = lookup(&id).expect("echo rule should be registered");
        assert!(std::ptr::eq(looked_up as *const (), echo_body as *const (),));
    }
}
