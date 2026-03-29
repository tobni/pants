// Copyright 2023 Pants project contributors (see CONTRIBUTORS.md).
// Licensed under the Apache License, Version 2.0 (see LICENSE).

mod field;
mod sources;
pub(crate) mod util;

pub(crate) use field::{AsyncFieldMixin, Field, ScalarField, SequenceField, StringSequenceField};
pub(crate) use sources::{
    MultipleSourcesField, OptionalSingleSourceField, SingleSourceField, SourcesField,
};

use field::NoFieldValue;

pub fn register(m: &pyo3::prelude::Bound<'_, pyo3::types::PyModule>) -> pyo3::PyResult<()> {
    use pyo3::prelude::*;

    m.add_class::<Field>()?;
    m.add_class::<ScalarField>()?;
    m.add_class::<SequenceField>()?;
    m.add_class::<StringSequenceField>()?;
    m.add_class::<AsyncFieldMixin>()?;
    m.add_class::<SourcesField>()?;
    m.add_class::<MultipleSourcesField>()?;
    m.add_class::<OptionalSingleSourceField>()?;
    m.add_class::<SingleSourceField>()?;
    m.add_class::<NoFieldValue>()?;

    m.add("NO_VALUE", NoFieldValue)?;

    Ok(())
}
