// Copyright 2021 Pants project contributors (see CONTRIBUTORS.md).
// Licensed under the Apache License, Version 2.0 (see LICENSE).

use std::borrow::Borrow;
use std::collections::HashMap;
use std::path::PathBuf;

use fs::{
    DigestTrie, DirectoryDigest, GlobMatching, PathGlobs, PathStat, RelativePath, SymlinkBehavior,
    TypedPath,
};
use hashing::{Digest, EMPTY_DIGEST};
use pyo3::prelude::{PyModule, PyRef, PyResult, Python, wrap_pyfunction};
use pyo3::types::{PyAnyMethods, PyModuleMethods, PyTuple, PyTypeMethods};
use pyo3::{Bound, IntoPyObject, PyAny};
use store::{SnapshotOps, SubsetParams};

use crate::externs;
use crate::externs::fs::{
    PyAddPrefix, PyFileDigest, PyMergeDigests, PyPathMetadata, PyPathNamespace, PyRemovePrefix,
};
use crate::nodes::{
    DownloadedFile, NodeResult, PathMetadataNode, Snapshot, SubjectPath, lift_directory_digest,
    task_get_context, unmatched_globs_additional_context,
};
use crate::python::{Key, Value, throw};
use crate::{Context, Failure};

pub fn register(_py: Python, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(py_bindings::add_prefix, m)?)?;
    m.add_function(wrap_pyfunction!(py_bindings::create_digest, m)?)?;
    m.add_function(wrap_pyfunction!(py_bindings::digest_subset_to_digest, m)?)?;
    m.add_function(wrap_pyfunction!(py_bindings::digest_to_snapshot, m)?)?;
    m.add_function(wrap_pyfunction!(py_bindings::get_digest_contents, m)?)?;
    m.add_function(wrap_pyfunction!(py_bindings::get_digest_entries, m)?)?;
    m.add_function(wrap_pyfunction!(py_bindings::download_file, m)?)?;
    m.add_function(wrap_pyfunction!(py_bindings::merge_digests, m)?)?;
    m.add_function(wrap_pyfunction!(py_bindings::path_globs_to_digest, m)?)?;
    m.add_function(wrap_pyfunction!(py_bindings::path_globs_to_paths, m)?)?;
    m.add_function(wrap_pyfunction!(py_bindings::remove_prefix, m)?)?;
    m.add_function(wrap_pyfunction!(py_bindings::path_metadata_request, m)?)?;

    Ok(())
}

pub async fn get_digest_contents(digest: Value) -> NodeResult<Value> {
    let context = task_get_context();

    let digest = Python::attach(|py| {
        let py_digest = digest.bind(py);
        lift_directory_digest(py_digest)
    })?;

    let digest_contents = context.core.store().contents_for_directory(digest).await?;

    Ok(Python::attach(|py| {
        Snapshot::store_digest_contents(py, &context, &digest_contents)
    })?)
}

pub async fn get_digest_entries(digest: Value) -> NodeResult<Value> {
    let context = task_get_context();

    let digest = Python::attach(|py| {
        let py_digest = digest.bind(py);
        lift_directory_digest(py_digest)
    })?;
    let digest_entries = context.core.store().entries_for_directory(digest).await?;
    Ok(Python::attach(|py| {
        Snapshot::store_digest_entries(py, &context, &digest_entries)
    })?)
}

pub async fn remove_prefix(remove_prefix: Value) -> NodeResult<Value> {
    let context = task_get_context();

    let (digest, prefix) = Python::attach(|py| {
        let py_remove_prefix = remove_prefix
            .bind(py)
            .extract::<PyRef<PyRemovePrefix>>()
            .map_err(|e| throw(format!("{e}")))?;
        let prefix = RelativePath::new(&py_remove_prefix.prefix)
            .map_err(|e| throw(format!("The `prefix` must be relative: {e}")))?;
        let res: NodeResult<_> = Ok((py_remove_prefix.digest.clone(), prefix));
        res
    })?;
    let digest = context.core.store().strip_prefix(digest, &prefix).await?;
    Ok(Python::attach(|py| {
        Snapshot::store_directory_digest(py, digest)
    })?)
}

pub async fn add_prefix(add_prefix: Value) -> NodeResult<Value> {
    let context = task_get_context();

    let (digest, prefix) = Python::attach(|py| {
        let py_add_prefix = add_prefix
            .bind(py)
            .extract::<PyRef<PyAddPrefix>>()
            .map_err(|e| throw(format!("{e}")))?;
        let prefix = RelativePath::new(&py_add_prefix.prefix)
            .map_err(|e| throw(format!("The `prefix` must be relative: {e}")))?;
        let res: NodeResult<(DirectoryDigest, RelativePath)> =
            Ok((py_add_prefix.digest.clone(), prefix));
        res
    })?;
    let digest = context.core.store().add_prefix(digest, &prefix).await?;
    Ok(Python::attach(|py| {
        Snapshot::store_directory_digest(py, digest)
    })?)
}

pub async fn digest_to_snapshot(digest: Value) -> NodeResult<Value> {
    let context = task_get_context();
    let store = context.core.store();

    let digest = Python::attach(|py| {
        let py_digest = digest.bind(py);
        lift_directory_digest(py_digest)
    })?;
    let snapshot = store::Snapshot::from_digest(store, digest).await?;
    Ok(Python::attach(|py| Snapshot::store_snapshot(py, snapshot))?)
}

pub async fn merge_digests(digests: Value) -> NodeResult<Value> {
    let context = task_get_context();

    let core = &context.core;
    let store = core.store();

    let digests = Python::attach(|py| {
        digests
            .bind(py)
            .extract::<PyRef<PyMergeDigests>>()
            .map(|py_merge_digests| py_merge_digests.0.clone())
            .map_err(|e| throw(format!("{e}")))
    })?;
    let digest = store.merge(digests).await?;
    Ok(Python::attach(|py| {
        Snapshot::store_directory_digest(py, digest)
    })?)
}

pub async fn download_file(download_file: Value) -> NodeResult<Value> {
    let context = task_get_context();

    let key = Key::from_value(download_file).map_err(Failure::from)?;
    let snapshot = context.get(DownloadedFile(key)).await?;
    Ok(Python::attach(|py| {
        Snapshot::store_directory_digest(py, snapshot.into())
    })?)
}

pub async fn path_globs_to_digest(path_globs: Value) -> NodeResult<Value> {
    let context = task_get_context();
    let digest = inner_path_globs_to_digest(path_globs, &context).await?;
    Ok(Python::attach(|py| {
        Snapshot::store_directory_digest(py, digest)
    })?)
}

async fn inner_path_globs_to_digest(
    path_globs: Value,
    context: &Context,
) -> Result<DirectoryDigest, Failure> {
    let path_globs = lift_python_path_globs(path_globs)?;
    Ok(context
        .get(Snapshot::from_path_globs(path_globs))
        .await?
        .into())
}

fn lift_python_path_globs(path_globs: Value) -> Result<PathGlobs, Failure> {
    Python::attach(|py| {
        let py_path_globs = path_globs.bind(py);
        Snapshot::lift_path_globs(py_path_globs)
    })
    .map_err(|e| throw(format!("Failed to parse PathGlobs: {e}")))
}

pub async fn path_globs_to_paths(path_globs: Value) -> NodeResult<Value> {
    let context = task_get_context();
    let core = &context.core;

    let path_globs = lift_python_path_globs(path_globs)?;

    let path_globs = path_globs.parse().map_err(throw)?;
    let path_stats = context
        .expand_globs(
            path_globs,
            SymlinkBehavior::Oblivious,
            unmatched_globs_additional_context(),
        )
        .await?;

    Python::attach(|py| {
        let mut files = Vec::new();
        let mut dirs = Vec::new();
        for ps in path_stats.iter() {
            match ps {
                PathStat::File { path, .. } => {
                    files.push(Snapshot::store_path(py, path)?);
                }
                PathStat::Link { path, .. } => {
                    panic!("Paths shouldn't be symlink-aware {path:?}");
                }
                PathStat::Dir { path, .. } => {
                    dirs.push(Snapshot::store_path(py, path)?);
                }
            }
        }
        Ok::<_, Failure>(externs::unsafe_call(
            py,
            core.types.paths,
            &[
                externs::store_tuple(py, files)?,
                externs::store_tuple(py, dirs)?,
            ],
        ))
    })
}

enum CreateDigestItem {
    FileContent(RelativePath, bytes::Bytes, bool),
    FileEntry(RelativePath, Digest, bool),
    SymlinkEntry(RelativePath, PathBuf),
    Dir(RelativePath),
}

pub async fn create_digest(create_digest: Value) -> NodeResult<Value> {
    let (items_to_store, trie) = {
        let mut new_file_count = 0;

        let items: Vec<CreateDigestItem> = Python::attach(|py| -> Result<_, Failure> {
            let py_create_digest = create_digest.bind(py);
            Ok(externs::collect_iterable(py_create_digest)
                .map_err(|e| {
                    throw(format!(
                        "Error while collecting CreateDigestItem instances: {e}"
                    ))
                })?
                .into_iter()
                .map(|obj| {
                    let raw_path: String = externs::getattr(&obj, "path").unwrap();
                    let path = RelativePath::new(PathBuf::from(raw_path)).unwrap();
                    if obj.hasattr("content").unwrap() {
                        let bytes = bytes::Bytes::from(
                            externs::getattr::<Vec<u8>>(&obj, "content").unwrap(),
                        );
                        let is_executable: bool = externs::getattr(&obj, "is_executable").unwrap();
                        new_file_count += 1;
                        CreateDigestItem::FileContent(path, bytes, is_executable)
                    } else if obj.hasattr("file_digest").unwrap() {
                        let py_file_digest: PyFileDigest =
                            externs::getattr(&obj, "file_digest").unwrap();
                        let is_executable: bool = externs::getattr(&obj, "is_executable").unwrap();
                        CreateDigestItem::FileEntry(path, py_file_digest.0, is_executable)
                    } else if obj.hasattr("target").unwrap() {
                        let target: String = externs::getattr(&obj, "target").unwrap();
                        CreateDigestItem::SymlinkEntry(path, PathBuf::from(target))
                    } else {
                        CreateDigestItem::Dir(path)
                    }
                })
                .collect())
        })?;

        let mut typed_paths: Vec<TypedPath> = Vec::with_capacity(items.len());
        let mut file_digests: HashMap<PathBuf, Digest> = HashMap::with_capacity(items.len());
        let mut items_to_store = Vec::with_capacity(new_file_count);

        for item in &items {
            match item {
                CreateDigestItem::FileContent(path, bytes, is_executable) => {
                    let digest = Digest::of_bytes(bytes);
                    items_to_store.push((digest.hash, bytes.clone()));
                    typed_paths.push(TypedPath::File {
                        path,
                        is_executable: *is_executable,
                    });
                    file_digests.insert(path.to_path_buf(), digest);
                }
                CreateDigestItem::FileEntry(path, digest, is_executable) => {
                    typed_paths.push(TypedPath::File {
                        path,
                        is_executable: *is_executable,
                    });
                    file_digests.insert(path.to_path_buf(), *digest);
                }
                CreateDigestItem::SymlinkEntry(path, target) => {
                    typed_paths.push(TypedPath::Link { path, target });
                    file_digests.insert(path.to_path_buf(), EMPTY_DIGEST);
                }
                CreateDigestItem::Dir(path) => {
                    typed_paths.push(TypedPath::Dir(path));
                    file_digests.insert(path.to_path_buf(), EMPTY_DIGEST);
                }
            }
        }

        let trie = DigestTrie::from_unique_paths(typed_paths, &file_digests)
            .map_err(|e| throw(format!("DigestTrie::from_unique_paths failure: {e}")))?;

        (items_to_store, trie)
    };

    let context = task_get_context();
    let store = context.core.store();
    store.store_file_bytes_batch(items_to_store, true).await?;
    Ok(Python::attach(|py| {
        Snapshot::store_directory_digest(py, trie.into())
    })?)
}

pub async fn digest_subset_to_digest(digest_subset: Value) -> NodeResult<Value> {
    let context = task_get_context();

    let store = context.core.store();
    let (path_globs, original_digest) = Python::attach(|py| {
        let py_digest_subset = digest_subset.bind(py);
        let py_path_globs: Bound<'_, PyAny> = externs::getattr(py_digest_subset, "globs").unwrap();
        let py_digest: Bound<'_, PyAny> = externs::getattr(py_digest_subset, "digest").unwrap();
        let res: NodeResult<_> = Ok((
            Snapshot::lift_prepared_path_globs(&py_path_globs)?,
            lift_directory_digest(&py_digest)?,
        ));
        res
    })?;
    let subset_params = SubsetParams { globs: path_globs };
    let digest = store.subset(original_digest, subset_params).await?;
    Ok(Python::attach(|py| {
        Snapshot::store_directory_digest(py, digest)
    })?)
}

pub async fn path_metadata_request(single_path: Value) -> NodeResult<Value> {
    let subject_path = Python::attach(|py| -> Result<_, String> {
        let arg = single_path.bind(py);
        let path = externs::getattr_as_optional_string(arg, "path")
            .map_err(|e| format!("Failed to get `path` for field: {e}"))?;
        let path = path.ok_or_else(|| "Path must not be `None`.".to_string())?;

        let namespace: PyPathNamespace = externs::getattr(arg, "namespace")
            .map_err(|e| format!("Failed to get `namespace` for field: {e}"))?;
        match namespace {
            PyPathNamespace::Workspace => SubjectPath::new_workspace(&path).map_err(|_| {
                format!("path_metadata_request error: path for PathNamespace.WORKSPACE must be a relative path. Instead, got `{path}`")
            }),
            PyPathNamespace::System => SubjectPath::new_system(&path).map_err(|_| {
                format!(
                    "path_metadata_request error: path for PathNamespace.SYSTEM must be an absolute path. Instead, got `{path}`"
                )
            }),
        }
    })?;

    let context = task_get_context();
    let metadata_opt = context
        .get(PathMetadataNode::new(subject_path)?)
        .await?
        .map(PyPathMetadata);

    Ok(Python::attach(|py| {
        let path_metadata_opt = match metadata_opt {
            Some(m) => m
                .into_pyobject(py)
                .unwrap_or_else(|e| {
                    panic!("Failed to convert type: {e:?}",);
                })
                .into_any(),
            None => py.None().into_bound(py),
        };

        let py_type = context.core.types.path_metadata_result.as_py_type(py);
        let args_tuple = PyTuple::new(py, &[path_metadata_opt]).unwrap_or_else(|e| {
            panic!("Core type constructor `PyTuple` failed: {e:?}");
        });
        let res = py_type.call1(args_tuple).unwrap_or_else(|e| {
            panic!(
                "Core type constructor `{}` failed: {:?}",
                py_type.borrow().name().unwrap(),
                e
            );
        });
        Value::from(&res)
    }))
}

mod py_bindings {
    use pyo3::pyfunction;

    use crate::externs::PyGeneratorResponseNativeCall;
    use crate::intrinsics::native_rule::native_call;
    use crate::python::Value;

    #[pyfunction]
    pub fn add_prefix(add_prefix: Value) -> PyGeneratorResponseNativeCall {
        native_call(add_prefix, super::add_prefix)
    }

    #[pyfunction]
    pub fn create_digest(create_digest: Value) -> PyGeneratorResponseNativeCall {
        native_call(create_digest, super::create_digest)
    }

    #[pyfunction]
    pub fn digest_subset_to_digest(digest_subset: Value) -> PyGeneratorResponseNativeCall {
        native_call(digest_subset, super::digest_subset_to_digest)
    }

    #[pyfunction]
    pub fn digest_to_snapshot(digest: Value) -> PyGeneratorResponseNativeCall {
        native_call(digest, super::digest_to_snapshot)
    }

    #[pyfunction]
    pub fn get_digest_contents(digest: Value) -> PyGeneratorResponseNativeCall {
        native_call(digest, super::get_digest_contents)
    }

    #[pyfunction]
    pub fn get_digest_entries(digest: Value) -> PyGeneratorResponseNativeCall {
        native_call(digest, super::get_digest_entries)
    }

    #[pyfunction]
    pub fn download_file(download_file: Value) -> PyGeneratorResponseNativeCall {
        native_call(download_file, super::download_file)
    }

    #[pyfunction]
    pub fn merge_digests(digests: Value) -> PyGeneratorResponseNativeCall {
        native_call(digests, super::merge_digests)
    }

    #[pyfunction]
    pub fn path_globs_to_digest(path_globs: Value) -> PyGeneratorResponseNativeCall {
        native_call(path_globs, super::path_globs_to_digest)
    }

    #[pyfunction]
    pub fn path_globs_to_paths(path_globs: Value) -> PyGeneratorResponseNativeCall {
        native_call(path_globs, super::path_globs_to_paths)
    }

    #[pyfunction]
    pub fn remove_prefix(remove_prefix: Value) -> PyGeneratorResponseNativeCall {
        native_call(remove_prefix, super::remove_prefix)
    }

    #[pyfunction]
    pub fn path_metadata_request(single_path: Value) -> PyGeneratorResponseNativeCall {
        native_call(single_path, super::path_metadata_request)
    }
}
