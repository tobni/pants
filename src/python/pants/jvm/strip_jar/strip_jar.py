# Copyright 2021 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).

import importlib.resources
from dataclasses import dataclass

from pants.core.goals.resolves import ExportableTool
from pants.engine.fs import AddPrefix, CreateDigest, Digest, Directory, FileContent
from pants.engine.internals.native_engine import MergeDigests, RemovePrefix
from pants.engine.intrinsics import add_prefix, create_digest, merge_digests, remove_prefix
from pants.engine.process import FallibleProcessResult, ProcessCacheScope, execute_process_or_raise
from pants.engine.rules import collect_rules, concurrently, implicitly, rule
from pants.engine.unions import UnionRule
from pants.jvm.jdk_rules import InternalJdk, JvmProcess
from pants.jvm.resolve.coursier_fetch import ToolClasspathRequest, materialize_classpath_for_tool
from pants.jvm.resolve.jvm_tool import GenerateJvmLockfileFromTool, JvmToolBase
from pants.util.logging import LogLevel

_STRIP_JAR_BASENAME = "StripJar.java"
_OUTPUT_PATH = "__stripped_jars"


class StripJarTool(JvmToolBase):
    options_scope = "strip-jar"
    help = "Reproducible Build Maven Plugin"

    default_version = "0.16"
    default_artifacts = ("io.github.zlika:reproducible-build-maven-plugin:{version}",)
    default_lockfile_resource = ("pants.jvm.strip_jar", "strip_jar.lock")


@dataclass(frozen=True)
class StripJarRequest:
    digest: Digest
    filenames: tuple[str, ...]


@dataclass(frozen=True)
class FallibleStripJarResult:
    process_result: FallibleProcessResult


@dataclass(frozen=True)
class StripJarCompiledClassfiles:
    digest: Digest


@rule(level=LogLevel.DEBUG)
async def strip_jar(
    processor_classfiles: StripJarCompiledClassfiles,
    jdk: InternalJdk,
    request: StripJarRequest,
    tool: StripJarTool,
) -> Digest:
    filenames = list(request.filenames)

    if len(filenames) == 0:
        return request.digest

    input_path = "__jars_to_strip"
    toolcp_relpath = "__toolcp"
    processorcp_relpath = "__processorcp"

    tool_classpath, prefixed_jars_digest = await concurrently(
        materialize_classpath_for_tool(
            ToolClasspathRequest(lockfile=GenerateJvmLockfileFromTool.create(tool))
        ),
        add_prefix(AddPrefix(request.digest, input_path)),
    )

    extra_immutable_input_digests = {
        toolcp_relpath: tool_classpath.digest,
        processorcp_relpath: processor_classfiles.digest,
    }

    process_result = await execute_process_or_raise(
        **implicitly(
            JvmProcess(
                jdk=jdk,
                classpath_entries=[
                    *tool_classpath.classpath_entries(toolcp_relpath),
                    processorcp_relpath,
                ],
                argv=["org.pantsbuild.stripjar.StripJar", input_path, _OUTPUT_PATH, *filenames],
                input_digest=prefixed_jars_digest,
                extra_immutable_input_digests=extra_immutable_input_digests,
                output_directories=(_OUTPUT_PATH,),
                extra_nailgun_keys=extra_immutable_input_digests,
                description=f"Stripping jar {filenames[0]}",
                level=LogLevel.DEBUG,
                cache_scope=ProcessCacheScope.LOCAL_SUCCESSFUL,
            )
        )
    )

    return await remove_prefix(RemovePrefix(process_result.output_digest, _OUTPUT_PATH))


def _load_strip_jar_source() -> bytes:
    parent_module = ".".join(__name__.split(".")[:-1])
    return importlib.resources.files(parent_module).joinpath(_STRIP_JAR_BASENAME).read_bytes()


# TODO(13879): Consolidate compilation of wrapper binaries to common rules.
@rule
async def build_processors(jdk: InternalJdk, tool: StripJarTool) -> StripJarCompiledClassfiles:
    dest_dir = "classfiles"
    materialized_classpath, source_digest = await concurrently(
        materialize_classpath_for_tool(
            ToolClasspathRequest(
                prefix="__toolcp", lockfile=GenerateJvmLockfileFromTool.create(tool)
            )
        ),
        create_digest(
            CreateDigest(
                [
                    FileContent(
                        path=_STRIP_JAR_BASENAME,
                        content=_load_strip_jar_source(),
                    ),
                    Directory(dest_dir),
                ]
            )
        ),
    )

    merged_digest = await merge_digests(
        MergeDigests(
            (
                materialized_classpath.digest,
                source_digest,
            )
        )
    )

    process_result = await execute_process_or_raise(
        **implicitly(
            JvmProcess(
                jdk=jdk,
                classpath_entries=[f"{jdk.java_home}/lib/tools.jar"],
                argv=[
                    "com.sun.tools.javac.Main",
                    "-cp",
                    ":".join(materialized_classpath.classpath_entries()),
                    "-d",
                    dest_dir,
                    _STRIP_JAR_BASENAME,
                ],
                input_digest=merged_digest,
                output_directories=(dest_dir,),
                description=f"Compile {_STRIP_JAR_BASENAME} with javac",
                level=LogLevel.DEBUG,
                # NB: We do not use nailgun for this process, since it is launched exactly once.
                use_nailgun=False,
            )
        )
    )
    stripped_classfiles_digest = await remove_prefix(
        RemovePrefix(process_result.output_digest, dest_dir)
    )
    return StripJarCompiledClassfiles(digest=stripped_classfiles_digest)


def rules():
    return (*collect_rules(), UnionRule(ExportableTool, StripJarTool))
