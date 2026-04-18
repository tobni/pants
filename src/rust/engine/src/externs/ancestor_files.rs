// Copyright 2026 Pants project contributors (see CONTRIBUTORS.md).
// Licensed under the Apache License, Version 2.0 (see LICENSE).

use std::sync::Arc;

use pyo3::prelude::{PyModule, PyResult, Python, pymethods};
use pyo3::pybacked::PyBackedStr;
use pyo3::types::{PyModuleMethods, PyTuple};
use pyo3::{Bound, Py, pyclass};

use crate::externs::fs::PySnapshot;

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<AncestorFilesRequest>()?;
    m.add_class::<AncestorFilesInDirRequest>()?;
    m.add_class::<AncestorFiles>()?;
    Ok(())
}

/// A request for ancestor files of the given names.
///
/// "Ancestor files" means all files with one of the given names that are siblings of, or in
/// parent directories of, a `.py` or `.pyi` file in the input_files.
#[pyclass(
    frozen,
    eq,
    hash,
    from_py_object,
    module = "pants.engine.internals.native_engine"
)]
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct AncestorFilesRequest {
    pub input_files: Arc<[PyBackedStr]>,
    pub requested: Arc<[PyBackedStr]>,
    pub ignore_empty_files: bool,
}

impl crate::intrinsics::rule_type::FrozenPyClassRuleType for AncestorFilesRequest {}

#[pymethods]
impl AncestorFilesRequest {
    #[new]
    #[pyo3(signature = (input_files, requested, ignore_empty_files=false))]
    fn new(
        input_files: Vec<PyBackedStr>,
        requested: Vec<PyBackedStr>,
        ignore_empty_files: bool,
    ) -> Self {
        Self {
            input_files: input_files.into(),
            requested: requested.into(),
            ignore_empty_files,
        }
    }

    #[getter]
    fn input_files<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(py, self.input_files.iter())
    }

    #[getter]
    fn requested<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(py, self.requested.iter())
    }

    #[getter]
    fn ignore_empty_files(&self) -> bool {
        self.ignore_empty_files
    }

    fn __repr__(&self) -> String {
        format!(
            "AncestorFilesRequest(input_files={:?}, requested={:?}, ignore_empty_files={})",
            self.input_files, self.requested, self.ignore_empty_files,
        )
    }
}

/// Per-directory slice of an `AncestorFilesRequest`. The `find_ancestor_files` dispatcher
/// splits its work into one sub-rule call per unique package directory so the rule graph
/// caches per-`(dir, requested, ignore_empty_files)` — a single physical
/// `__init__.py` lookup is shared across every caller whose `input_files` reach that dir.
#[pyclass(
    frozen,
    eq,
    hash,
    from_py_object,
    module = "pants.engine.internals.native_engine"
)]
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct AncestorFilesInDirRequest {
    // `dir` is always Rust-constructed by the dispatcher, so it's a plain owned string rather
    // than a `PyBackedStr`. `requested` mirrors the outer `AncestorFilesRequest` so the Arc can
    // be shared across every sub-request in one dispatcher invocation without copying.
    pub dir: Arc<str>,
    pub requested: Arc<[PyBackedStr]>,
    pub ignore_empty_files: bool,
}

impl crate::intrinsics::rule_type::FrozenPyClassRuleType for AncestorFilesInDirRequest {}

#[pymethods]
impl AncestorFilesInDirRequest {
    #[new]
    #[pyo3(signature = (dir, requested, ignore_empty_files=false))]
    fn new(dir: String, requested: Vec<PyBackedStr>, ignore_empty_files: bool) -> Self {
        Self {
            dir: dir.into(),
            requested: requested.into(),
            ignore_empty_files,
        }
    }

    #[getter]
    fn dir(&self) -> &str {
        &self.dir
    }

    #[getter]
    fn requested<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(py, self.requested.iter())
    }

    #[getter]
    fn ignore_empty_files(&self) -> bool {
        self.ignore_empty_files
    }

    fn __repr__(&self) -> String {
        format!(
            "AncestorFilesInDirRequest(dir={:?}, requested={:?}, ignore_empty_files={})",
            self.dir, self.requested, self.ignore_empty_files,
        )
    }
}

/// Any ancestor files found.
#[pyclass(
    frozen,
    from_py_object,
    module = "pants.engine.internals.native_engine"
)]
pub struct AncestorFiles {
    pub snapshot: Py<PySnapshot>,
}

// `Py<T>` only clones under the GIL, so `derive(Clone)` wouldn't do the right thing. We
// attach briefly here — AncestorFiles is returned to callers via `Value::from(py_obj)` in
// practice, so actual clone traffic is low.
impl Clone for AncestorFiles {
    fn clone(&self) -> Self {
        Self {
            snapshot: Python::attach(|py| self.snapshot.clone_ref(py)),
        }
    }
}

impl crate::intrinsics::rule_type::FrozenPyClassRuleType for AncestorFiles {}

#[pymethods]
impl AncestorFiles {
    #[new]
    fn new(snapshot: Py<PySnapshot>) -> Self {
        Self { snapshot }
    }

    #[getter]
    fn snapshot(&self) -> &Py<PySnapshot> {
        &self.snapshot
    }

    fn __eq__(&self, other: &Self) -> bool {
        self.snapshot.get().0.digest == other.snapshot.get().0.digest
    }

    fn __hash__(&self) -> u64 {
        self.snapshot.get().0.digest.hash.prefix_hash()
    }

    fn __repr__(&self) -> String {
        format!(
            "AncestorFiles(snapshot={})",
            self.snapshot.get().0.digest.hash.to_hex()
        )
    }
}
