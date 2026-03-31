# Copyright 2024 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).

from __future__ import annotations

import dataclasses
import logging

from pants.engine.environment import EnvironmentName
from pants.engine.fs import (
    AddPrefix,
    CreateDigest,
    Digest,
    DigestContents,
    DigestEntries,
    DigestSubset,
    MergeDigests,
    NativeDownloadFile,
    PathGlobs,
    PathMetadataRequest,
    PathMetadataResult,
    Paths,
    RemovePrefix,
    Snapshot,
)
from pants.engine.internals import native_engine
from pants.engine.internals.docker import DockerResolveImageRequest, DockerResolveImageResult
from pants.engine.internals.native_dep_inference import (
    NativeDockerfileInfos,
    NativeJavascriptFilesDependencies,
    NativePythonFilesDependencies,
)
from pants.engine.internals.native_engine import NativeDependenciesRequest, task_side_effected
from pants.engine.internals.session import RunId, SessionValues
from pants.engine.process import (
    FallibleProcessResult,
    InteractiveProcess,
    InteractiveProcessResult,
    Process,
    ProcessExecutionEnvironment,
    ProcessResultWithRetries,
    ProcessWithRetries,
)
from pants.engine.rules import (
    _uncacheable_rule,
    collect_rules,
    implicitly,
    native_rule,
    rule,
)
from pants.util.docutil import git_url
from pants.util.frozendict import FrozenDict


# Native rules: pure passthroughs to Rust intrinsics.
# The engine calls the #[pyfunction] once to get a NativeCall future,
# then awaits it directly -- skipping the generator protocol.
create_digest = native_rule(native_engine.create_digest, CreateDigest, Digest)
path_globs_to_digest = native_rule(native_engine.path_globs_to_digest, PathGlobs, Digest)
path_globs_to_paths = native_rule(native_engine.path_globs_to_paths, PathGlobs, Paths)
download_file = native_rule(native_engine.download_file, NativeDownloadFile, Digest)
digest_to_snapshot = native_rule(native_engine.digest_to_snapshot, Digest, Snapshot)
get_digest_contents = native_rule(native_engine.get_digest_contents, Digest, DigestContents)
get_digest_entries = native_rule(native_engine.get_digest_entries, Digest, DigestEntries)
merge_digests = native_rule(native_engine.merge_digests, MergeDigests, Digest)
remove_prefix = native_rule(native_engine.remove_prefix, RemovePrefix, Digest)
add_prefix = native_rule(native_engine.add_prefix, AddPrefix, Digest)
digest_subset_to_digest = native_rule(
    native_engine.digest_subset_to_digest, DigestSubset, Digest
)
path_metadata_request = native_rule(
    native_engine.path_metadata_request, PathMetadataRequest, PathMetadataResult
)
docker_resolve_image = native_rule(
    native_engine.docker_resolve_image, DockerResolveImageRequest, DockerResolveImageResult
)


# Rules with real Python logic -- cannot be native_rule.
@rule
async def execute_process(
    process: Process, process_execution_environment: ProcessExecutionEnvironment
) -> FallibleProcessResult:
    return await native_engine.execute_process(process, process_execution_environment)


@rule
async def execute_process_with_retry(req: ProcessWithRetries) -> ProcessResultWithRetries:
    results: list[FallibleProcessResult] = []
    for attempt in range(0, req.attempts):
        proc = dataclasses.replace(req.proc, attempt=attempt)
        result = await execute_process(proc, **implicitly())
        results.append(result)
        if result.exit_code == 0:
            break
    return ProcessResultWithRetries(tuple(results))


@rule
async def session_values() -> SessionValues:
    return await native_engine.session_values()


@rule
async def run_id() -> RunId:
    return await native_engine.run_id()


__SQUELCH_WARNING = "__squelch_warning"


# NB: Call one of the helpers below, instead of calling this rule directly,
#  to ensure correct application of restartable logic.
@_uncacheable_rule
async def _interactive_process(
    process: InteractiveProcess, process_execution_environment: ProcessExecutionEnvironment
) -> InteractiveProcessResult:
    # This is a crafty way for a caller to signal into this function without a dedicated arg
    # (which would confound the solver).  Note that we go via __dict__ instead of using
    # setattr/delattr, because those error for frozen dataclasses.
    if __SQUELCH_WARNING in process.__dict__:
        del process.__dict__[__SQUELCH_WARNING]
    else:
        logging.warning(
            "A plugin is calling `await _interactive_process(...)` directly. This will cause "
            "restarting logic not to be applied. Use `await run_interactive_process(process)` "
            "or `await run_interactive_process_in_environment(process, environment_name)` instead. "
            f"See {git_url('src/python/pants/engine/intrinsics.py')} for more details."
        )
    return await native_engine.interactive_process(process, process_execution_environment)


async def run_interactive_process(process: InteractiveProcess) -> InteractiveProcessResult:
    # NB: We must call task_side_effected() in this helper, rather than in a nested @rule call,
    #  so that the Task for the @rule that calls this helper is the one marked as non-restartable.
    if not process.restartable:
        task_side_effected()

    process.__dict__[__SQUELCH_WARNING] = True
    ret: InteractiveProcessResult = await _interactive_process(process, **implicitly())
    return ret


async def run_interactive_process_in_environment(
    process: InteractiveProcess, environment_name: EnvironmentName
) -> InteractiveProcessResult:
    # NB: We must call task_side_effected() in this helper, rather than in a nested @rule call,
    #  so that the Task for the @rule that calls this helper is the one marked as non-restartable.
    if not process.restartable:
        task_side_effected()

    process.__dict__[__SQUELCH_WARNING] = True
    ret: InteractiveProcessResult = await _interactive_process(
        process, **implicitly({environment_name: EnvironmentName})
    )
    return ret


@rule
async def parse_dockerfile_info(
    deps_request: NativeDependenciesRequest,
) -> NativeDockerfileInfos:
    path_infos_pairs = await native_engine.parse_dockerfile_info(deps_request)
    return NativeDockerfileInfos(FrozenDict(path_infos_pairs))


@rule
async def parse_python_deps(
    deps_request: NativeDependenciesRequest,
) -> NativePythonFilesDependencies:
    path_deps_pairs = await native_engine.parse_python_deps(deps_request)
    return NativePythonFilesDependencies(FrozenDict(path_deps_pairs))


@rule
async def parse_javascript_deps(
    deps_request: NativeDependenciesRequest,
) -> NativeJavascriptFilesDependencies:
    path_deps_pairs = await native_engine.parse_javascript_deps(deps_request)
    return NativeJavascriptFilesDependencies(FrozenDict(path_deps_pairs))


def rules():
    return [
        *collect_rules(),
    ]
