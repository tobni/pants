// Copyright 2026 Pants project contributors (see CONTRIBUTORS.md).
// Licensed under the Apache License, Version 2.0 (see LICENSE).

use std::sync::OnceLock;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PySet, PyTuple};

static INVALID_FIELD_TYPE_EXCEPTION: OnceLock<Py<PyAny>> = OnceLock::new();
static INVALID_FIELD_CHOICE_EXCEPTION: OnceLock<Py<PyAny>> = OnceLock::new();
static INVALID_FIELD_EXCEPTION: OnceLock<Py<PyAny>> = OnceLock::new();

pub fn raise_invalid_field_type(
    py: Python,
    address: &Bound<PyAny>,
    alias: &str,
    raw_value: Option<&Bound<PyAny>>,
    expected_type_desc: &str,
) -> PyErr {
    let get_exc = || -> PyResult<Bound<PyAny>> {
        if let Some(exc) = INVALID_FIELD_TYPE_EXCEPTION.get() {
            return Ok(exc.bind(py).clone());
        }
        let exc = py
            .import("pants.engine.target")?
            .getattr("InvalidFieldTypeException")?;
        let _ = INVALID_FIELD_TYPE_EXCEPTION.set(exc.clone().unbind());
        Ok(exc)
    };
    match get_exc() {
        Ok(exc_cls) => {
            let kwargs = PyDict::new(py);
            let _ = kwargs.set_item("expected_type", expected_type_desc);
            match exc_cls.call((address, alias, raw_value), Some(&kwargs)) {
                Ok(exc) => PyErr::from_value(exc),
                Err(e) => e,
            }
        }
        Err(e) => e,
    }
}

pub fn validate_choices(
    py: Python,
    address: &Bound<PyAny>,
    alias: &str,
    values: &Bound<PyAny>,
    valid_choices: &Bound<PyAny>,
) -> PyResult<()> {
    let choices_set = PySet::empty(py)?;
    if valid_choices.is_instance_of::<PyTuple>() {
        for item in valid_choices.try_iter()? {
            choices_set.add(item?)?;
        }
    } else {
        for member in valid_choices.try_iter()? {
            let member = member?;
            choices_set.add(member.getattr(intern!(py, "value"))?)?;
        }
    }
    for choice in values.try_iter()? {
        let choice = choice?;
        if !choices_set.contains(&choice)? {
            let get_exc = || -> PyResult<Bound<PyAny>> {
                if let Some(exc) = INVALID_FIELD_CHOICE_EXCEPTION.get() {
                    return Ok(exc.bind(py).clone());
                }
                let exc = py
                    .import("pants.engine.target")?
                    .getattr("InvalidFieldChoiceException")?;
                let _ = INVALID_FIELD_CHOICE_EXCEPTION.set(exc.clone().unbind());
                Ok(exc)
            };
            let exc_cls = get_exc()?;
            let kwargs = PyDict::new(py);
            kwargs.set_item("valid_choices", &choices_set)?;
            return Err(PyErr::from_value(
                exc_cls.call((address, alias, &choice), Some(&kwargs))?,
            ));
        }
    }
    Ok(())
}

pub fn raise_invalid_field_exception(py: Python, message: &str) -> PyErr {
    let get_exc = || -> PyResult<Bound<PyAny>> {
        if let Some(exc) = INVALID_FIELD_EXCEPTION.get() {
            return Ok(exc.bind(py).clone());
        }
        let exc = py
            .import("pants.engine.target")?
            .getattr("InvalidFieldException")?;
        let _ = INVALID_FIELD_EXCEPTION.set(exc.clone().unbind());
        Ok(exc)
    };
    match get_exc() {
        Ok(exc_cls) => match exc_cls.call1((message,)) {
            Ok(exc) => PyErr::from_value(exc),
            Err(e) => e,
        },
        Err(e) => e,
    }
}

pub struct DisplayStrList<'a, S: AsRef<str>>(pub &'a [S]);

impl<S: AsRef<str>> std::fmt::Display for DisplayStrList<'_, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[")?;
        for (i, s) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "'{}'", s.as_ref())?;
        }
        f.write_str("]")
    }
}
