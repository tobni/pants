// Copyright 2026 Pants project contributors (see CONTRIBUTORS.md).
// Licensed under the Apache License, Version 2.0 (see LICENSE).

use std::collections::HashSet;
use std::path::Path;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::pyclass_init::PyClassInitializer;
use pyo3::types::{PyDict, PyString, PyTuple, PyType};

use crate::externs::address::Address;
use crate::externs::fs::{
    PyFilespecMatcher, PyGlobExpansionConjunction, PyGlobMatchErrorBehavior, PyPathGlobs,
};
use crate::externs::unions::UnionMembership;

use super::field::{AsyncFieldMixin, Field, ScalarField, StringSequenceField};
use super::util::{DisplayStrList, raise_invalid_field_exception};

fn join_to_string(base: &Path, path: &str) -> String {
    base.join(path).to_string_lossy().into_owned()
}

fn prefix_glob(dirpath: &Path, glob: &str) -> String {
    if let Some(rest) = glob.strip_prefix('!') {
        format!("!{}", join_to_string(dirpath, rest))
    } else {
        join_to_string(dirpath, glob)
    }
}

fn split_globs(spec_path: &Path, globs: &[String]) -> (Vec<String>, Vec<String>) {
    let mut includes = Vec::new();
    let mut excludes = Vec::new();
    for glob in globs {
        if let Some(rest) = glob.strip_prefix('!') {
            excludes.push(join_to_string(spec_path, rest));
        } else {
            includes.push(join_to_string(spec_path, glob));
        }
    }
    (includes, excludes)
}

fn pluralize_files(n: usize) -> String {
    if n == 1 {
        "1 file".to_string()
    } else {
        format!("{n} files")
    }
}

fn validate_file_extensions(
    files: &[String],
    extensions: &[String],
    alias: &str,
    address: &str,
) -> Option<String> {
    let mut bad_files: Vec<&str> = files
        .iter()
        .filter(|fp| {
            let suffix = Path::new(fp.as_str())
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();
            !extensions.contains(&suffix)
        })
        .map(|s| s.as_str())
        .collect();
    if bad_files.is_empty() {
        return None;
    }
    bad_files.sort();
    let expected = if extensions.len() > 1 {
        let mut sorted = extensions.to_vec();
        sorted.sort();
        format!("one of {}", DisplayStrList(&sorted))
    } else {
        format!("'{}'", extensions[0])
    };
    Some(format!(
        "The {alias:?} field in target {address} can only contain \
         files that end in {expected}, but it had these files: {}.\
         \n\nMaybe create a `resource`/`resources` or `file`/`files` target and \
         include it in the `dependencies` field?",
        DisplayStrList(&bad_files)
    ))
}

fn validate_num_files(
    expected_num: &Bound<'_, PyAny>,
    num_files: usize,
    alias: &str,
    address: &str,
    py: Python,
) -> PyResult<()> {
    let is_bad = if expected_num.is_instance_of::<pyo3::types::PyRange>() {
        !expected_num.contains(num_files)?
    } else {
        expected_num.extract::<usize>()? != num_files
    };
    if !is_bad {
        return Ok(());
    }
    let expected_str = if expected_num.is_instance_of::<pyo3::types::PyRange>() {
        let range_list: Vec<usize> = expected_num
            .try_iter()?
            .map(|i| i.unwrap().extract().unwrap())
            .collect();
        if range_list.len() == 2 {
            format!("{} or {} files", range_list[0], range_list[1])
        } else {
            format!("a number of files in the range `{expected_num}`")
        }
    } else {
        pluralize_files(expected_num.extract()?)
    };
    Err(raise_invalid_field_exception(
        py,
        &format!(
            "The {alias:?} field in target {address} must have \
             {expected_str}, but it had {}.",
            pluralize_files(num_files)
        ),
    ))
}

fn new_path_globs(
    globs: Vec<String>,
    behavior: PyGlobMatchErrorBehavior,
    description: Option<String>,
    py: Python,
) -> PyResult<Py<PyAny>> {
    Ok(Py::new(
        py,
        PyPathGlobs::create(
            globs,
            behavior,
            PyGlobExpansionConjunction::AnyMatch,
            description,
            py,
        )?,
    )?
    .into_any())
}

fn extract_default_globs(default: &Bound<'_, PyAny>) -> Option<Vec<String>> {
    if default.is_none() {
        None
    } else if let Ok(s) = default.extract::<String>() {
        Some(vec![s])
    } else {
        default.extract().ok()
    }
}

fn globs_match_defaults(globs: &[String], defaults: &Option<Vec<String>>) -> bool {
    defaults
        .as_ref()
        .map(|dg| {
            let gs: HashSet<&str> = globs.iter().map(|s| s.as_str()).collect();
            let ds: HashSet<&str> = dg.iter().map(|s| s.as_str()).collect();
            gs == ds
        })
        .unwrap_or(false)
}

fn resolve_error_behavior(
    cls: &Bound<'_, PyType>,
    globs: &[String],
    unmatched_build_file_globs: &Bound<'_, PyAny>,
    py: Python,
) -> PyResult<PyGlobMatchErrorBehavior> {
    let default = cls.getattr(intern!(py, "default"))?;
    let default_globs = extract_default_globs(&default);
    let using_defaults = globs_match_defaults(globs, &default_globs);

    let override_behavior = cls.getattr(intern!(py, "default_glob_match_error_behavior"))?;
    let source = if using_defaults && !override_behavior.is_none() {
        override_behavior
    } else {
        unmatched_build_file_globs.getattr(intern!(py, "error_behavior"))?
    };
    Ok(source.extract::<PyGlobMatchErrorBehavior>()?)
}

fn validate_multiple_sources_globs(
    globs: &[String],
    ban_subdirs: bool,
    alias: &str,
    address: &str,
) -> Option<String> {
    if globs
        .iter()
        .any(|g| g.starts_with("../") || g.contains("/../"))
    {
        let mut sorted = globs.to_vec();
        sorted.sort();
        return Some(format!(
            "The {alias:?} field in target {address} must not have globs with the \
             pattern `../` because targets can only have sources in the current directory \
             or subdirectories. It was set to: {}",
            DisplayStrList(&sorted)
        ));
    }
    if ban_subdirs && globs.iter().any(|g| g.contains("**") || g.contains('/')) {
        let mut sorted = globs.to_vec();
        sorted.sort();
        return Some(format!(
            "The {alias:?} field in target {address} must only have globs for \
             the target's directory, i.e. it cannot include values with `**` or \
             `/`. It was set to: {}",
            DisplayStrList(&sorted)
        ));
    }
    None
}

fn validate_single_source(val: &str, alias: &str, address: &str) -> Option<String> {
    if val.starts_with("../") || val.contains("/../") {
        return Some(format!(
            "The {alias:?} field in target {address} should not include `../` \
             patterns because targets can only have sources in the current directory or \
             subdirectories. It was set to {val}. Instead, use a normalized \
             literal file path (relative to the BUILD file).",
        ));
    }
    if val.contains('*') {
        return Some(format!(
            "The {alias:?} field in target {address} should not include `*` globs, \
             but was set to {val}. Instead, use a literal file path (relative \
             to the BUILD file).",
        ));
    }
    if val.starts_with('!') {
        return Some(format!(
            "The {alias:?} field in target {address} should not start with `!`, \
             which is usually used in the `sources` field to exclude certain files. \
             Instead, use a literal file path (relative to the BUILD file).",
        ));
    }
    None
}

fn field_value<'py>(self_: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let field = self_.cast::<Field>()?;
    Ok(field.get().value.bind(self_.py()).clone())
}

fn field_address<'py>(self_: &Bound<'py, PyAny>) -> PyResult<Bound<'py, Address>> {
    let afm = self_.cast::<AsyncFieldMixin>()?;
    Ok(afm.get().address.bind(self_.py()).clone())
}

static GENERATE_SOURCES_REQUEST: std::sync::OnceLock<Py<PyAny>> = std::sync::OnceLock::new();

fn get_generate_sources_request<'py>(py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
    if let Some(gsr) = GENERATE_SOURCES_REQUEST.get() {
        return Ok(gsr.bind(py).clone());
    }
    let gsr = py
        .import("pants.engine.target")?
        .getattr("GenerateSourcesRequest")?;
    let _ = GENERATE_SOURCES_REQUEST.set(gsr.clone().unbind());
    Ok(gsr)
}

#[pyclass(subclass, frozen, extends = AsyncFieldMixin, module = "pants.engine.internals.native_engine")]
pub(crate) struct SourcesField;

#[pymethods]
impl SourcesField {
    #[new]
    #[classmethod]
    #[pyo3(signature = (raw_value, address))]
    fn __new__(
        cls: &Bound<'_, PyType>,
        raw_value: Option<&Bound<'_, PyAny>>,
        address: Bound<'_, Address>,
        py: Python,
    ) -> PyResult<PyClassInitializer<Self>> {
        let mixin = AsyncFieldMixin::__new__(cls, raw_value, address, py)?;
        Ok(mixin.add_subclass(Self))
    }

    #[classattr]
    fn expected_file_extensions<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
        py.None().into_bound(py)
    }

    #[classattr]
    fn expected_num_files<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
        py.None().into_bound(py)
    }

    #[classattr]
    fn uses_source_roots() -> bool {
        true
    }

    #[classattr]
    fn default<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
        py.None().into_bound(py)
    }

    #[classattr]
    fn default_glob_match_error_behavior<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
        py.None().into_bound(py)
    }

    #[getter]
    fn globs<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        Ok(PyTuple::empty(py))
    }

    fn validate_resolved_files(
        self_: &Bound<'_, Self>,
        files: Vec<String>,
        py: Python,
    ) -> PyResult<()> {
        let cls = self_.get_type();
        let address = field_address(self_.as_any())?;
        let address_str = address.to_string();
        let alias: String = Field::cls_alias(&cls)?;

        let expected_extensions = cls.getattr(intern!(py, "expected_file_extensions"))?;
        if !expected_extensions.is_none() {
            let extensions: Vec<String> = expected_extensions.extract()?;
            if let Some(msg) = py.detach(|| -> PyResult<_> {
                Ok(validate_file_extensions(
                    &files,
                    &extensions,
                    &alias,
                    &address_str,
                ))
            })? {
                return Err(raise_invalid_field_exception(py, &msg));
            }
        }

        let expected_num = cls.getattr(intern!(py, "expected_num_files"))?;
        if !expected_num.is_none() {
            validate_num_files(&expected_num, files.len(), &alias, &address_str, py)?;
        }

        Ok(())
    }

    #[staticmethod]
    fn prefix_glob_with_dirpath(dirpath: &str, glob: &str) -> String {
        prefix_glob(Path::new(dirpath), glob)
    }

    fn _prefix_glob_with_address(self_: &Bound<'_, Self>, glob: &str) -> PyResult<String> {
        let address = field_address(self_.as_any())?;
        Ok(prefix_glob(address.get().spec_path_ref(), glob))
    }

    #[classmethod]
    fn can_generate(
        cls: &Bound<'_, PyType>,
        output_type: &Bound<'_, PyType>,
        union_membership: &Bound<'_, UnionMembership>,
        py: Python,
    ) -> PyResult<bool> {
        let gsr_any = get_generate_sources_request(py)?;
        let gsr_type = gsr_any.cast::<PyType>()?;
        let members = union_membership
            .get()
            .get_members_tuple(gsr_type)
            .unwrap_or_else(|| Ok(PyTuple::empty(py)))?;
        for member in members.iter() {
            let input_any = member.getattr(intern!(py, "input"))?;
            let output_any = member.getattr(intern!(py, "output"))?;
            if cls.is_subclass(input_any.cast::<PyType>()?)?
                && output_any.cast::<PyType>()?.is_subclass(output_type)?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn path_globs(
        self_: &Bound<'_, Self>,
        unmatched_build_file_globs: &Bound<'_, PyAny>,
        py: Python,
    ) -> PyResult<Py<PyAny>> {
        let globs: Vec<String> = self_.getattr(intern!(py, "globs"))?.extract()?;
        if globs.is_empty() {
            return new_path_globs(vec![], PyGlobMatchErrorBehavior::Ignore, None, py);
        }

        let cls = self_.get_type();
        let address = field_address(self_.as_any())?;
        let spec_path = address.get().spec_path_ref();

        let prefixed: Vec<String> = py.detach(|| -> PyResult<_> {
            Ok(globs.iter().map(|g| prefix_glob(spec_path, g)).collect())
        })?;

        let error_behavior = resolve_error_behavior(&cls, &globs, unmatched_build_file_globs, py)?;

        let description = if error_behavior == PyGlobMatchErrorBehavior::Ignore {
            None
        } else {
            let alias: String = Field::cls_alias(&cls)?;
            Some(format!("{address}'s `{alias}` field"))
        };

        new_path_globs(prefixed, error_behavior, description, py)
    }

    #[getter]
    fn filespec(self_: &Bound<'_, Self>, py: Python) -> PyResult<Py<PyAny>> {
        let globs: Vec<String> = self_.getattr(intern!(py, "globs"))?.extract()?;
        let address = field_address(self_.as_any())?;
        let spec_path = address.get().spec_path_ref();

        let (includes, excludes) =
            py.detach(|| -> PyResult<_> { Ok(split_globs(spec_path, &globs)) })?;

        let result = PyDict::new(py);
        result.set_item("includes", includes)?;
        if !excludes.is_empty() {
            result.set_item("excludes", excludes)?;
        }
        Ok(result.unbind().into_any())
    }

    #[getter]
    fn filespec_matcher(self_: &Bound<'_, Self>, py: Python) -> PyResult<Py<PyAny>> {
        let globs: Vec<String> = self_.getattr(intern!(py, "globs"))?.extract()?;
        let address = field_address(self_.as_any())?;
        let spec_path = address.get().spec_path_ref();

        let (includes, excludes) =
            py.detach(|| -> PyResult<_> { Ok(split_globs(spec_path, &globs)) })?;

        Ok(Py::new(py, PyFilespecMatcher::create(includes, excludes, py)?)?.into_any())
    }
}

#[pyclass(subclass, frozen, extends = SourcesField, module = "pants.engine.internals.native_engine")]
pub(crate) struct MultipleSourcesField;

#[pymethods]
impl MultipleSourcesField {
    #[new]
    #[classmethod]
    #[pyo3(signature = (raw_value, address))]
    fn __new__(
        cls: &Bound<'_, PyType>,
        raw_value: Option<&Bound<'_, PyAny>>,
        address: Bound<'_, Address>,
        py: Python,
    ) -> PyResult<PyClassInitializer<Self>> {
        let mixin = AsyncFieldMixin::__new__(cls, raw_value, address, py)?;
        Ok(mixin.add_subclass(SourcesField).add_subclass(Self))
    }

    #[classattr]
    fn alias() -> &'static str {
        "sources"
    }

    #[classattr]
    fn expected_element_type<'py>(py: Python<'py>) -> Bound<'py, PyType> {
        py.get_type::<PyString>()
    }

    #[classattr]
    fn expected_type_description() -> &'static str {
        "an iterable of strings (e.g. a list of strings)"
    }

    #[classattr]
    fn valid_choices<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
        py.None().into_bound(py)
    }

    #[classattr]
    fn ban_subdirectories() -> bool {
        false
    }

    #[getter]
    fn globs(self_: &Bound<'_, Self>) -> PyResult<Py<PyAny>> {
        let py = self_.py();
        let value = field_value(self_.as_any())?;
        if value.is_none() {
            return Ok(PyTuple::empty(py).unbind().into_any());
        }
        Ok(value.unbind())
    }

    #[classmethod]
    #[pyo3(signature = (raw_value, address))]
    fn compute_value<'py>(
        cls: &Bound<'py, PyType>,
        raw_value: Option<&Bound<'py, PyAny>>,
        address: Bound<'py, Address>,
        py: Python<'py>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let value = StringSequenceField::compute_value(cls, raw_value, address.clone(), py)?;
        if value.is_none() {
            return Ok(value);
        }

        let globs: Vec<String> = value.extract()?;
        let alias = Field::cls_alias(cls)?;
        let address_str = address.to_string();
        let ban_subdirs: bool = cls.getattr(intern!(py, "ban_subdirectories"))?.extract()?;

        if let Some(msg) = py.detach(|| -> PyResult<_> {
            Ok(validate_multiple_sources_globs(
                &globs,
                ban_subdirs,
                &alias,
                &address_str,
            ))
        })? {
            return Err(raise_invalid_field_exception(py, &msg));
        }
        Ok(value)
    }
}

#[pyclass(subclass, frozen, extends = SourcesField, module = "pants.engine.internals.native_engine")]
pub(crate) struct OptionalSingleSourceField;

#[pymethods]
impl OptionalSingleSourceField {
    #[new]
    #[classmethod]
    #[pyo3(signature = (raw_value, address))]
    fn __new__(
        cls: &Bound<'_, PyType>,
        raw_value: Option<&Bound<'_, PyAny>>,
        address: Bound<'_, Address>,
        py: Python,
    ) -> PyResult<PyClassInitializer<Self>> {
        let mixin = AsyncFieldMixin::__new__(cls, raw_value, address, py)?;
        Ok(mixin.add_subclass(SourcesField).add_subclass(Self))
    }

    #[classattr]
    fn alias() -> &'static str {
        "source"
    }

    #[classattr]
    fn expected_type<'py>(py: Python<'py>) -> Bound<'py, PyType> {
        py.get_type::<PyString>()
    }

    #[classattr]
    fn expected_type_description() -> &'static str {
        "a string"
    }

    #[classattr]
    fn help() -> &'static str {
        "A single file that belongs to this target.\n\n\
         Path is relative to the BUILD file's directory, e.g. `source='example.ext'`."
    }

    #[classattr]
    fn required() -> bool {
        false
    }

    #[classattr]
    fn default<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
        py.None().into_bound(py)
    }

    #[classattr]
    fn expected_num_files(py: Python) -> PyResult<Py<PyAny>> {
        Ok(pyo3::types::PyRange::new(py, 0, 2)?.unbind().into_any())
    }

    #[getter]
    fn globs(self_: &Bound<'_, Self>) -> PyResult<Py<PyAny>> {
        let py = self_.py();
        let value = field_value(self_.as_any())?;
        if value.is_none() {
            return Ok(PyTuple::empty(py).unbind().into_any());
        }
        Ok(PyTuple::new(py, [&value])?.unbind().into_any())
    }

    #[getter]
    fn file_path(self_: &Bound<'_, Self>) -> PyResult<Py<PyAny>> {
        let py = self_.py();
        let value = field_value(self_.as_any())?;
        if value.is_none() {
            return Ok(py.None());
        }
        let val: String = value.extract()?;
        let address = field_address(self_.as_any())?;
        Ok(
            PyString::new(py, &join_to_string(address.get().spec_path_ref(), &val))
                .unbind()
                .into_any(),
        )
    }

    #[classmethod]
    #[pyo3(signature = (raw_value, address))]
    fn compute_value<'py>(
        cls: &Bound<'py, PyType>,
        raw_value: Option<&Bound<'py, PyAny>>,
        address: Bound<'py, Address>,
        py: Python<'py>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let value_or_default = ScalarField::compute_value(cls, raw_value, address.clone(), py)?;
        if value_or_default.is_none() {
            return Ok(value_or_default);
        }

        let val: String = value_or_default.extract()?;
        let alias = Field::cls_alias(cls)?;
        let address_str = address.to_string();

        if let Some(msg) =
            py.detach(|| -> PyResult<_> { Ok(validate_single_source(&val, &alias, &address_str)) })?
        {
            return Err(raise_invalid_field_exception(py, &msg));
        }
        Ok(value_or_default)
    }
}

#[pyclass(subclass, frozen, extends = OptionalSingleSourceField, module = "pants.engine.internals.native_engine")]
pub(crate) struct SingleSourceField;

#[pymethods]
impl SingleSourceField {
    #[new]
    #[classmethod]
    #[pyo3(signature = (raw_value, address))]
    fn __new__(
        cls: &Bound<'_, PyType>,
        raw_value: Option<&Bound<'_, PyAny>>,
        address: Bound<'_, Address>,
        py: Python,
    ) -> PyResult<PyClassInitializer<Self>> {
        let mixin = AsyncFieldMixin::__new__(cls, raw_value, address, py)?;
        Ok(mixin
            .add_subclass(SourcesField)
            .add_subclass(OptionalSingleSourceField)
            .add_subclass(Self))
    }

    #[classattr]
    fn required() -> bool {
        true
    }

    #[classattr]
    fn expected_num_files() -> i64 {
        1
    }

    #[getter]
    fn file_path(self_: &Bound<'_, Self>) -> PyResult<String> {
        let value: String = field_value(self_.as_any())?.extract()?;
        let address = field_address(self_.as_any())?;
        Ok(join_to_string(address.get().spec_path_ref(), &value))
    }
}
