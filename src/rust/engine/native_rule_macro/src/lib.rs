// Copyright 2026 Pants project contributors (see CONTRIBUTORS.md).
// Licensed under the Apache License, Version 2.0 (see LICENSE).

//! `#[native_rule]` — declare a Rust async fn as a first-class rule in the pants engine.
//!
//! Applied to a function of the shape
//!
//! ```ignore
//! #[native_rule]
//! pub async fn path_globs_to_digest(globs: PathGlobs) -> NodeResult<DirectoryDigest> {
//!     /* body */
//! }
//! ```
//!
//! The macro rewrites the function as the cached, graph-dispatched public entry point and
//! emits hidden helpers: `__<name>_body` (the original body wrapped as a `NativeRuleFn`),
//! `__<name>_signature`, `__<name>_queries` (AST-extracted `implicitly` pairs), and
//! `__<name>_register`. At runtime every caller goes through `ctx.get(Task{...})`, giving
//! Python-`@rule`-equivalent caching.

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{format_ident, quote};
use syn::visit::Visit;
use syn::{FnArg, GenericArgument, ItemFn, Pat, PathArguments, ReturnType, Type};

/// The attribute macro. Takes no arguments.
#[proc_macro_attribute]
pub fn native_rule(attr: TokenStream, item: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return syn::Error::new(Span::call_site(), "#[native_rule] takes no arguments")
            .to_compile_error()
            .into();
    }

    let input: ItemFn = match syn::parse(item) {
        Ok(f) => f,
        Err(e) => return e.to_compile_error().into(),
    };

    match expand(input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand(input: ItemFn) -> syn::Result<proc_macro2::TokenStream> {
    if input.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            &input.sig.fn_token,
            "#[native_rule] requires an `async fn`",
        ));
    }

    let vis = &input.vis;
    let attrs = &input.attrs;
    let name = &input.sig.ident;
    let generics = &input.sig.generics;
    let inputs = &input.sig.inputs;
    let output = &input.sig.output;
    let block = &input.block;

    let body_fn = format_ident!("__{}_body", name);
    let signature_fn = format_ident!("__{}_signature", name);
    let queries_fn = format_ident!("__{}_queries", name);
    let register_fn = format_ident!("__{}_register", name);
    let impl_mod = format_ident!("__{}_impl", name);
    let rule_id_literal = format!("native::{}", name);

    // Scan the body AST for `implicitly(...)` calls so we can emit a matching
    // `QueryRule(O, [I])` registration — analogous to what `rule_visitor.py` does for Python
    // `@rule` bodies. Two passes: first populate the typed-locals map (so calls in a tail
    // position can resolve `I` from a `let ident: I` declared anywhere in scope, not just
    // before them), then collect the `(I, O)` pairs. Unresolved calls are promoted to compile
    // errors instead of runtime "no entry for output" failures.
    let (implicit_pairs, collector_errors) = {
        let mut locals = TypedLocalsCollector::default();
        locals.visit_block(block);
        let mut collector = ImplicitlyCollector {
            typed_locals: locals.typed_locals,
            ..ImplicitlyCollector::default()
        };
        collector.visit_block(block);
        (collector.calls, collector.errors)
    };
    if !collector_errors.is_empty() {
        let mut iter = collector_errors.into_iter();
        let mut combined = iter.next().unwrap();
        for e in iter {
            combined.combine(e);
        }
        return Err(combined);
    }
    let implicit_inputs: Vec<_> = implicit_pairs.iter().map(|(i, _)| i.clone()).collect();
    let implicit_outputs: Vec<_> = implicit_pairs.iter().map(|(_, o)| o.clone()).collect();

    // Extract (arg_name, arg_type) pairs from the signature.
    let mut arg_idents = Vec::new();
    let mut arg_names = Vec::new();
    let mut arg_types = Vec::new();
    for arg in inputs.iter() {
        match arg {
            FnArg::Typed(pat_type) => {
                let ident = match &*pat_type.pat {
                    Pat::Ident(pi) => pi.ident.clone(),
                    _ => {
                        return Err(syn::Error::new_spanned(
                            &pat_type.pat,
                            "#[native_rule] arguments must be simple identifiers",
                        ));
                    }
                };
                arg_names.push(ident.to_string());
                arg_idents.push(ident);
                arg_types.push((*pat_type.ty).clone());
            }
            FnArg::Receiver(_) => {
                return Err(syn::Error::new_spanned(
                    arg,
                    "#[native_rule] cannot be applied to methods",
                ));
            }
        }
    }

    // Pull the product type (O) out of `NodeResult<O>`.
    let product_ty = match output {
        ReturnType::Type(_, ty) => extract_node_result_ok(ty)?,
        ReturnType::Default => {
            return Err(syn::Error::new_spanned(
                &input.sig,
                "#[native_rule] requires `-> NodeResult<T>`",
            ));
        }
    };

    let arg_indices: Vec<usize> = (0..arg_idents.len()).collect();

    // Public entry point: goes through the graph so the result is Task-cached exactly like a
    // Python `@rule` call. The parameter is an `impl RuleArg<Input>` rather than `Input` so
    // callers can pass a ready value *or* `implicitly(other_value)` without an inner `.await?`.
    let public_fn = if arg_idents.len() == 1 {
        let arg0 = &arg_idents[0];
        let arg_ty = &arg_types[0];
        quote! {
            #(#attrs)*
            #vis async fn #name #generics (
                #arg0: impl crate::intrinsics::rule_type::RuleArg<#arg_ty>,
            ) #output {
                use crate::intrinsics::rule_type::RuleArg;
                let __input: #arg_ty = #arg0.resolve().await?;
                crate::intrinsics::rule_type::implicitly(__input).await
            }
        }
    } else {
        // For multi-input rules, `implicitly` on a tuple would be the natural surface. Not yet
        // supported — surface an error at call time rather than silently dropping caching.
        return Err(syn::Error::new_spanned(
            &input.sig,
            "#[native_rule] with multiple inputs is not yet supported — the single-arg form \
             dispatches through `implicitly`; multi-arg rules need a tuple `RuleType` impl.",
        ));
    };

    let expanded = quote! {
        #public_fn

        #[doc(hidden)]
        #[allow(non_snake_case)]
        mod #impl_mod {
            use super::*;
            pub(super) async fn inner(#inputs) #output #block
        }

        #[doc(hidden)]
        #[allow(non_snake_case)]
        fn #body_fn(
            deps: ::std::vec::Vec<crate::python::Value>,
        ) -> ::futures::future::BoxFuture<
            'static,
            crate::nodes::NodeResult<crate::python::Value>,
        > {
            use ::futures::FutureExt;
            async move {
                assert_eq!(
                    deps.len(),
                    #({ let _ = #arg_indices; 1 }+)* 0,
                    "Native rule received wrong number of deps",
                );
                let mut __iter = deps.into_iter();
                #(
                    let #arg_idents = ::pyo3::Python::attach(|py| {
                        <#arg_types as crate::intrinsics::rule_type::RuleType>::lift(
                            py,
                            &__iter.next().unwrap(),
                        )
                    })?;
                )*
                let __out = #impl_mod::inner(#(#arg_idents),*).await?;
                ::pyo3::Python::attach(|py| {
                    <#product_ty as crate::intrinsics::rule_type::RuleType>::store(__out, py)
                })
            }
            .boxed()
        }

        #[doc(hidden)]
        #[allow(non_snake_case)]
        fn #signature_fn(
            py: ::pyo3::Python<'_>,
        ) -> crate::native_rules::NativeRuleSignature {
            crate::native_rules::NativeRuleSignature {
                name: #rule_id_literal.to_string(),
                product: <#product_ty as crate::intrinsics::rule_type::RuleType>::python_type_id(py),
                args: vec![#(
                    (
                        #arg_names.to_string(),
                        <#arg_types as crate::intrinsics::rule_type::RuleType>::python_type_id(py),
                    ),
                )*],
                masked_types: vec![],
                desc: None,
                level: ::log::Level::Trace,
            }
        }

        /// Query-rule registrations derived from `implicitly::<I, O>` calls in the body.
        /// Each entry is a `(product, [params])` pair that `install_into` feeds to
        /// `Tasks::query_add` so the rule graph can solve `I → O` chains at build time.
        #[doc(hidden)]
        #[allow(non_snake_case)]
        fn #queries_fn(
            py: ::pyo3::Python<'_>,
        ) -> ::std::vec::Vec<(
            crate::python::TypeId,
            ::std::vec::Vec<crate::python::TypeId>,
        )> {
            vec![#(
                (
                    <#implicit_outputs as crate::intrinsics::rule_type::RuleType>::python_type_id(py),
                    vec![<#implicit_inputs as crate::intrinsics::rule_type::RuleType>::python_type_id(py)],
                ),
            )*]
        }

        #[doc(hidden)]
        #[allow(non_snake_case)]
        pub(crate) fn #register_fn() {
            crate::native_rules::register(
                crate::native_rules::NativeRuleRegistration {
                    rule_id: ::rule_graph::RuleId::new(#rule_id_literal),
                    body: #body_fn,
                    signature: #signature_fn,
                    queries: #queries_fn,
                },
            );
        }
    };

    Ok(expanded)
}

/// AST visitor that finds `implicitly(...)` calls inside a rule body and records the
/// `(I, O)` type pair — the macro uses them to auto-emit `QueryRule` registrations.
///
/// Forms recognized (any combination of explicit-vs-inferred):
/// - Turbofish for I, turbofish or let-binding for O: `let x: O = implicitly::<I, _>(...)`.
/// - No turbofish, input is a typed let-binding: `let g: I = ...; let x: O = implicitly(g)...`.
///
/// Each path the visitor takes ends up with a concrete `(I, O)` Type pair. If neither is
/// resolvable at macro time (e.g. both I and O come from surrounding inference), the call is
/// skipped and the user gets a runtime "no entry for output" error pointing to the fix.
/// Pre-pass that scans every `let <ident>: <Type> = ...;` across all nested blocks so the
/// main visitor can resolve `implicitly(<ident>)`'s `I` regardless of declaration order.
#[derive(Default)]
struct TypedLocalsCollector {
    typed_locals: std::collections::HashMap<syn::Ident, Type>,
}

impl<'ast> Visit<'ast> for TypedLocalsCollector {
    fn visit_local(&mut self, local: &'ast syn::Local) {
        if let syn::Pat::Type(pat_type) = &local.pat
            && let syn::Pat::Ident(pat_ident) = &*pat_type.pat
        {
            self.typed_locals
                .insert(pat_ident.ident.clone(), (*pat_type.ty).clone());
        }
        syn::visit::visit_local(self, local);
    }
}

#[derive(Default)]
struct ImplicitlyCollector {
    calls: Vec<(Type, Type)>,
    /// Tracks `let <ident>: <Type> = ...;` bindings so `implicitly(<ident>)` can read `I` from
    /// the binding's annotation. Simple flat scope — sufficient for rule bodies, which rarely
    /// shadow or rebind.
    typed_locals: std::collections::HashMap<syn::Ident, Type>,
    /// Compile-time errors for `implicitly(...)` calls the visitor couldn't fully resolve.
    /// Returned alongside the collected pairs so the attribute macro can fail the expansion
    /// rather than let an unregistered `implicitly` chain blow up at runtime.
    errors: Vec<syn::Error>,
    /// Addresses of `ExprCall` nodes already processed by `walk_tail_expr` with an `O` hint.
    /// The default `visit_expr_call` skips these so we don't double-record or spuriously
    /// error on a call whose `O` was already resolved via an enclosing typed `let`.
    handled: std::collections::HashSet<usize>,
}

impl ImplicitlyCollector {
    /// Try to parse `expr` as an `implicitly(...)` call and extract `(turbofish_I, turbofish_O,
    /// arg_expr)`. Returns `None` if `expr` isn't an `implicitly` call.
    fn parse_implicitly(expr: &syn::Expr) -> Option<(Option<Type>, Option<Type>, &syn::Expr)> {
        let call = if let syn::Expr::Call(c) = expr {
            c
        } else {
            return None;
        };
        let expr_path = if let syn::Expr::Path(p) = &*call.func {
            p
        } else {
            return None;
        };
        let last = expr_path.path.segments.last()?;
        if last.ident != "implicitly" {
            return None;
        }
        let (i_turbo, o_turbo) = if let PathArguments::AngleBracketed(ab) = &last.arguments {
            let mut types = ab.args.iter().filter_map(|a| match a {
                GenericArgument::Type(t) => Some(t.clone()),
                _ => None,
            });
            let i = types.next().filter(|t| !matches!(t, Type::Infer(_)));
            let o = types.next().filter(|t| !matches!(t, Type::Infer(_)));
            (i, o)
        } else {
            (None, None)
        };
        // Pull the single positional arg's expression so callers can try to resolve I from it.
        let arg = call.args.first()?;
        Some((i_turbo, o_turbo, arg))
    }

    /// Strip `.await?` / `.await` / `?` suffix layers to reach the inner call expression.
    fn peel_suffixes(expr: &syn::Expr) -> &syn::Expr {
        let mut cur = expr;
        loop {
            match cur {
                syn::Expr::Try(t) => cur = &t.expr,
                syn::Expr::Await(a) => cur = &a.base,
                _ => return cur,
            }
        }
    }

    /// If `expr` is a bare identifier that matches a previously-seen typed `let` binding,
    /// return that binding's declared type.
    fn type_of_ident(&self, expr: &syn::Expr) -> Option<Type> {
        let path = if let syn::Expr::Path(p) = expr {
            p
        } else {
            return None;
        };
        if path.path.segments.len() != 1 {
            return None;
        }
        let ident = &path.path.segments.first()?.ident;
        self.typed_locals.get(ident).cloned()
    }

    /// Record `(I, O)` for an implicitly call if we can resolve both types. `O` comes from the
    /// turbofish or `outer_o_hint` (set when the call is the init of a typed `let`). `I` comes
    /// from the turbofish or from looking up the arg's identifier in the typed-locals table.
    /// If either can't be resolved, push a `syn::Error` pointed at the call — the attribute
    /// macro returns it as a compile error so the rule author fixes the source, rather than
    /// hitting a runtime "no entry for output" failure.
    fn record(&mut self, expr: &syn::Expr, outer_o_hint: Option<Type>) {
        let Some((i_turbo, o_turbo, arg)) = Self::parse_implicitly(expr) else {
            return;
        };
        let i = i_turbo.or_else(|| self.type_of_ident(arg));
        let o = o_turbo.or(outer_o_hint);
        match (i, o) {
            (Some(i), Some(o)) => self.calls.push((i, o)),
            (i_opt, o_opt) => {
                let mut missing = Vec::new();
                if i_opt.is_none() {
                    missing.push("input type `I`");
                }
                if o_opt.is_none() {
                    missing.push("output type `O`");
                }
                self.errors.push(syn::Error::new_spanned(
                    expr,
                    format!(
                        "`implicitly` call in a `#[native_rule]` body needs {} to be resolvable \
                         at macro time — use `implicitly::<I, O>(...)` turbofish, or a typed \
                         `let <ident>: I = ...;` binding for the input and `let x: O = ...` for \
                         the output. The macro emits a `QueryRule(O, [I])` registration and can \
                         only do so when both types are spelled out in source.",
                        missing.join(" and "),
                    ),
                ));
            }
        }
    }
}

impl ImplicitlyCollector {
    /// Recursively walk an expression's tail positions looking for `implicitly(...)` calls
    /// that should inherit the given `outer_o` hint. Handles `.await?` / `?` suffixes plus
    /// the structural tails of `if`/`match`/`block`/`paren` — anywhere Rust propagates a let's
    /// declared type into a trailing expression, the macro resolves `O` from it.
    fn walk_tail_expr(&mut self, expr: &syn::Expr, outer_o: &Type) {
        let inner = Self::peel_suffixes(expr);
        match inner {
            syn::Expr::If(if_expr) => {
                if let Some(syn::Stmt::Expr(tail, None)) = if_expr.then_branch.stmts.last() {
                    self.walk_tail_expr(tail, outer_o);
                }
                if let Some((_, else_expr)) = &if_expr.else_branch {
                    self.walk_tail_expr(else_expr, outer_o);
                }
            }
            syn::Expr::Block(b) => {
                if let Some(syn::Stmt::Expr(tail, None)) = b.block.stmts.last() {
                    self.walk_tail_expr(tail, outer_o);
                }
            }
            syn::Expr::Match(m) => {
                for arm in &m.arms {
                    self.walk_tail_expr(&arm.body, outer_o);
                }
            }
            syn::Expr::Paren(p) => self.walk_tail_expr(&p.expr, outer_o),
            syn::Expr::Call(call) => {
                if Self::parse_implicitly(inner).is_some() {
                    self.record(inner, Some(outer_o.clone()));
                    self.handled.insert(call as *const _ as usize);
                }
            }
            _ => {}
        }
    }
}

impl<'ast> Visit<'ast> for ImplicitlyCollector {
    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        // `implicitly(x)` in function-argument position — e.g. `rule(implicitly(x))` — has its
        // `O` inferred from the outer call's `impl RuleArg<O>` parameter, and the hop it
        // represents is covered by the auto-registered `QueryRule` of whichever rule produces
        // `O` from `I`. Skip recording those so the macro doesn't spuriously error on a
        // perfectly-resolvable callsite.
        for arg in &call.args {
            let inner = Self::peel_suffixes(arg);
            if let syn::Expr::Call(inner_call) = inner
                && Self::parse_implicitly(inner).is_some()
            {
                self.handled.insert(inner_call as *const _ as usize);
            }
        }
        if !self.handled.contains(&(call as *const _ as usize)) {
            self.record(&syn::Expr::Call(call.clone()), None);
        }
        syn::visit::visit_expr_call(self, call);
    }

    fn visit_local(&mut self, local: &'ast syn::Local) {
        // typed_locals is pre-populated by `TypedLocalsCollector`; here we just propagate the
        // let's declared type into any `implicitly` tail expressions of the init — directly or
        // through `if`/`match`/`block` branches.
        if let syn::Pat::Type(pat_type) = &local.pat
            && let Some(init) = &local.init
        {
            self.walk_tail_expr(&init.expr, &pat_type.ty);
        }
        syn::visit::visit_local(self, local);
    }
}

/// Given a `Type` that is `NodeResult<X>` (i.e. `Result<X, Failure>` via the alias), return `X`.
fn extract_node_result_ok(ty: &Type) -> syn::Result<Type> {
    if let Type::Path(p) = ty {
        let last =
            p.path.segments.last().ok_or_else(|| {
                syn::Error::new_spanned(ty, "expected `NodeResult<T>` return type")
            })?;
        if last.ident != "NodeResult" {
            return Err(syn::Error::new_spanned(
                &last.ident,
                "expected `NodeResult<T>` return type",
            ));
        }
        if let PathArguments::AngleBracketed(ab) = &last.arguments {
            for arg in &ab.args {
                if let GenericArgument::Type(t) = arg {
                    return Ok(t.clone());
                }
            }
        }
    }
    Err(syn::Error::new_spanned(
        ty,
        "could not extract `T` from `NodeResult<T>`",
    ))
}
