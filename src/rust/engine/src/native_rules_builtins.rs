// Copyright 2026 Pants project contributors (see CONTRIBUTORS.md).
// Licensed under the Apache License, Version 2.0 (see LICENSE).

//! Built-in `#[native_rule]` registrations.
//!
//! Each `#[native_rule]`-decorated fn in this module becomes a first-class rule in the engine
//! rule graph — `Task`-cached, dispatched through `ctx.get(Task{...})`, callable from Python
//! via the auto-installed `RuleCallTrampoline`, callable from Rust via the macro-generated
//! public entry point that routes through `implicitly`. The `register_all()` call wires up
//! every `__<name>_register` helper the macro emits.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use fs::{
    DirectoryDigest, GlobExpansionConjunction, PathGlobs, PreparedPathGlobs, StrictGlobMatching,
};
use futures::future::try_join_all;
use native_rule_macro::native_rule;
use pyo3::Py;
use store::{SnapshotOps, SubsetParams};

use crate::externs::ancestor_files::{
    AncestorFiles, AncestorFilesInDirRequest, AncestorFilesRequest,
};
use crate::externs::fs::PySnapshot;
use crate::intrinsics::digests;
use crate::intrinsics::rule_type::implicitly;
use crate::nodes::{NodeResult, Snapshot, task_get_context};
use crate::python::Failure;

static INSTALLED: OnceLock<()> = OnceLock::new();

/// Install every built-in native rule into the global registry exactly once per process. Safe
/// to call from every `scheduler_create` / `#[pymodule]` init — the `OnceLock` guarantees
/// idempotency.
pub fn register_all() {
    INSTALLED.get_or_init(|| {
        __path_globs_to_digest_register();
        __digest_to_snapshot_register();
        __ancestor_files_in_dir_register();
        __find_ancestor_files_register();
    });
}

/// `PathGlobs → Digest`. Rust-native, no Python frame.
#[native_rule]
pub async fn path_globs_to_digest(path_globs: PathGlobs) -> NodeResult<DirectoryDigest> {
    let ctx = task_get_context();
    Ok(ctx.get(Snapshot::from_path_globs(path_globs)).await?.into())
}

/// `Digest → Snapshot`. Materializes a `Snapshot` from an already-persisted digest.
#[native_rule]
pub async fn digest_to_snapshot(digest: DirectoryDigest) -> NodeResult<store::Snapshot> {
    let ctx = task_get_context();
    store::Snapshot::from_digest(ctx.core.store(), digest)
        .await
        .map_err(Failure::from)
}

/// Per-package slice of `find_ancestor_files`. Globs `{dir}/{name}` for each requested name in
/// one directory and returns the resulting `AncestorFiles`. Split out so the rule graph caches
/// per `(dir, requested, ignore_empty_files)` — a single physical `__init__.py` lookup then
/// serves every outer request whose `input_files` land in that dir.
#[native_rule]
pub async fn ancestor_files_in_dir(
    request: AncestorFilesInDirRequest,
) -> NodeResult<AncestorFiles> {
    let globs_list: Vec<String> = request
        .requested
        .iter()
        .map(|name| {
            if request.dir.is_empty() {
                name.to_string()
            } else {
                format!("{}/{}", request.dir, name)
            }
        })
        .collect();
    let globs: PathGlobs = path_globs(globs_list);

    let snapshot: store::Snapshot = if request.ignore_empty_files {
        let initial: DirectoryDigest = implicitly(globs).await?;
        let contents = digests::get_digest_contents(initial).await?;
        let kept: Vec<String> = contents
            .iter()
            .filter(|fc| !fc.content.iter().all(u8::is_ascii_whitespace))
            .map(|fc| fc.path.display().to_string())
            .collect();
        if kept.is_empty() {
            store::Snapshot::empty()
        } else {
            let kept_globs: PathGlobs = path_globs(kept);
            digest_to_snapshot(implicitly(kept_globs)).await?
        }
    } else {
        digest_to_snapshot(implicitly(globs)).await?
    };

    wrap_snapshot(snapshot)
}

/// `AncestorFilesRequest → AncestorFiles`. Dispatcher only: expands the input files into the
/// set of package directories that must be searched, fans out one `ancestor_files_in_dir` call
/// per directory so the expensive globbing is cached per-dir, merges the resulting digests,
/// and subtracts `input_files` from the final output to match the original contract.
#[native_rule]
pub async fn find_ancestor_files(request: AncestorFilesRequest) -> NodeResult<AncestorFiles> {
    let dirs = unique_package_dirs(&request.input_files);
    if dirs.is_empty() {
        return wrap_snapshot(store::Snapshot::empty());
    }

    let per_dir_results: Vec<AncestorFiles> = try_join_all(dirs.into_iter().map(|dir| {
        // Arc clones are refcount bumps: one requested list is shared across every sub-request.
        ancestor_files_in_dir(AncestorFilesInDirRequest {
            dir: dir.into(),
            requested: request.requested.clone(),
            ignore_empty_files: request.ignore_empty_files,
        })
    }))
    .await?;

    let digests: Vec<DirectoryDigest> = per_dir_results
        .into_iter()
        .map(|af| af.snapshot.get().0.clone().into())
        .collect();

    let ctx = task_get_context();
    let store = ctx.core.store();
    let merged = store.merge(digests).await.map_err(Failure::from)?;

    let final_digest = if request.input_files.is_empty() {
        merged
    } else {
        // Subset-by-exclusion removes input_files from the merged trie in-memory — cheap
        // compared to re-globbing from the filesystem.
        let mut globs: Vec<String> = Vec::with_capacity(request.input_files.len() + 1);
        globs.push("**".to_string());
        for f in request.input_files.iter() {
            globs.push(format!("!{f}"));
        }
        let prepared = PreparedPathGlobs::create(
            globs,
            StrictGlobMatching::Ignore,
            GlobExpansionConjunction::AnyMatch,
        )
        .map_err(|e| Failure::from(format!("prepare subset globs: {e}")))?;
        store
            .subset(merged, SubsetParams { globs: prepared })
            .await
            .map_err(Failure::from)?
    };

    let snapshot = store::Snapshot::from_digest(store, final_digest)
        .await
        .map_err(Failure::from)?;
    wrap_snapshot(snapshot)
}

fn wrap_snapshot(snapshot: store::Snapshot) -> NodeResult<AncestorFiles> {
    let py_snapshot = pyo3::Python::attach(|py| -> NodeResult<Py<PySnapshot>> {
        Py::new(py, PySnapshot(snapshot))
            .map_err(|e| Failure::from(format!("Failed to construct Snapshot: {e}")))
    })?;
    Ok(AncestorFiles {
        snapshot: py_snapshot,
    })
}

/// All package directories reachable by walking the ancestry of each `.py`/`.pyi` input file.
/// Each directory becomes one `ancestor_files_in_dir` cache entry — the finest granularity we
/// can cache at, so two callers whose `input_files` share a common prefix reuse each other's
/// per-dir lookups.
fn unique_package_dirs<S: AsRef<str>>(input_files: &[S]) -> BTreeSet<String> {
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    for input_file in input_files {
        let input_file = input_file.as_ref();
        if !input_file.ends_with(".py") && !input_file.ends_with(".pyi") {
            continue;
        }
        let pkg_dir = match input_file.rfind('/') {
            Some(idx) => &input_file[..idx],
            None => "",
        };
        if dirs.contains(pkg_dir) {
            continue;
        }
        let mut dir = String::new();
        dirs.insert(dir.clone());
        for component in pkg_dir.split('/') {
            if component.is_empty() {
                continue;
            }
            if !dir.is_empty() {
                dir.push('/');
            }
            dir.push_str(component);
            dirs.insert(dir.clone());
        }
    }
    dirs
}

fn path_globs(globs: Vec<String>) -> PathGlobs {
    PathGlobs::new(
        globs,
        StrictGlobMatching::Ignore,
        GlobExpansionConjunction::AnyMatch,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(input_files: &[&str]) -> Vec<String> {
        unique_package_dirs(input_files).into_iter().collect()
    }

    #[test]
    fn walks_full_ancestry_per_leaf_package() {
        assert_eq!(
            dirs(&[
                "a/b/foo.py",
                "a/b/c/__init__.py",
                "a/b/c/d/bar.py",
                "a/e/__init__.py",
            ]),
            vec!["", "a", "a/b", "a/b/c", "a/b/c/d", "a/e"]
        );
    }

    #[test]
    fn walks_full_path_from_root() {
        assert_eq!(
            dirs(&[
                "src/python/a/b/foo.py",
                "src/python/a/b/c/__init__.py",
                "src/python/a/b/c/d/bar.py",
                "src/python/a/e/__init__.py",
            ]),
            vec![
                "",
                "src",
                "src/python",
                "src/python/a",
                "src/python/a/b",
                "src/python/a/b/c",
                "src/python/a/b/c/d",
                "src/python/a/e",
            ]
        );
    }

    #[test]
    fn skips_non_python_inputs() {
        assert!(dirs(&["a/b/foo.txt"]).is_empty());
    }

    #[test]
    fn dedupes_shared_ancestors() {
        assert_eq!(
            dirs(&["x/y/a.py", "x/y/b.py", "x/z/c.py"]),
            vec!["", "x", "x/y", "x/z"]
        );
    }
}
