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

use deepsize::known_deep_size;
use futures::future::BoxFuture;
use log::Level;
use pyo3::Python;
use rule_graph::RuleId;

use crate::nodes::NodeResult;
use crate::python::{TypeId, Value};

/// A native rule body after dispatch. Receives the resolved dependency values in the same order
/// they were declared during registration; returns the rule's product value.
pub type NativeRuleFn = fn(Vec<Value>) -> BoxFuture<'static, NodeResult<Value>>;

/// A `NativeRuleFn` wrapped so it can be stored on the interned `tasks::Task`, which derives
/// `DeepSizeOf`/`Hash`/`Eq`. A bare `fn` pointer has no `DeepSizeOf` impl, and deriving
/// `PartialEq`/`Hash` over one trips the `unpredictable_function_pointer_comparisons` lint; the
/// newtype supplies all three by hand (a function pointer owns no heap, and is compared/hashed by
/// address — sufficient here since `native_fn` is fully determined by the task's `id`).
#[derive(Clone, Copy, Debug)]
pub struct NativeRuleFnPtr(pub NativeRuleFn);

impl PartialEq for NativeRuleFnPtr {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::fn_addr_eq(self.0, other.0)
    }
}

impl Eq for NativeRuleFnPtr {}

impl std::hash::Hash for NativeRuleFnPtr {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (self.0 as *const () as usize).hash(state);
    }
}

known_deep_size!(8; NativeRuleFnPtr);

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
            reg.body,
            signature.name,
            signature.desc,
            signature.level,
        );
        // Wire each `implicitly::<I, O>` hop the macro found in the body as a `Get` on this rule,
        // so `resolve_implicit` can dispatch it through the rule's own edges — carrying the rule's
        // params and `@union` scope, the way a Python `@rule`'s `Get` resolves. Hops the macro
        // couldn't see (e.g. reached through another rule's inlined public fn) fall back to the
        // free-standing roots below.
        for (product, params) in (reg.queries)(py) {
            tasks.add_get(product, params);
        }
        tasks.task_end();
        // The rule's own `(input → output)` as a queryable root, so external (Python) callers can
        // reach it and so indirect hops resolve via `RuleGraph::find_root`.
        tasks.query_add(signature.product, param_types);
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
        // Fail loudly on a name clash (another native rule, or anything already on the module)
        // rather than silently last-writer-wins overwriting an existing attribute/intrinsic.
        if m.hasattr(short_name)? {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "native rule trampoline name {short_name:?} (from rule {}) collides with an \
                 existing attribute on the `native_engine` module",
                reg.rule_id
            )));
        }
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
    fn register_roundtrip() {
        let id = RuleId::new("native_rules::test_echo");
        register(NativeRuleRegistration {
            rule_id: id.clone(),
            body: echo_body,
            signature: echo_signature,
            queries: echo_queries,
        });
        let reg = iter_registrations()
            .into_iter()
            .find(|r| r.rule_id == id)
            .expect("echo rule should be registered");
        assert!(std::ptr::fn_addr_eq(reg.body, echo_body as NativeRuleFn));
    }
}
