# Copyright 2019 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).

from __future__ import annotations

import logging
from dataclasses import dataclass

from pants.backend.google_cloud_function.python.target_types import (
    PythonGoogleCloudFunction,
    PythonGoogleCloudFunctionHandlerField,
    PythonGoogleCloudFunctionRuntime,
    PythonGoogleCloudFunctionType,
)
from pants.backend.python.util_rules.faas import (
    BuildPythonFaaSRequest,
    FaaSArchitecture,
    PythonFaaSCompletePlatforms,
    PythonFaaSLayoutField,
    PythonFaaSPex3VenvCreateExtraArgsField,
    PythonFaaSPexBuildExtraArgs,
    build_python_faas,
)
from pants.backend.python.util_rules.faas import rules as faas_rules
from pants.core.environments.target_types import EnvironmentField
from pants.core.goals.package import BuiltPackage, OutputPathField, PackageFieldSet
from pants.engine.rules import collect_rules, rule
from pants.engine.unions import UnionRule
from pants.util.logging import LogLevel

logger = logging.getLogger(__name__)


@dataclass(frozen=True)
class PythonGoogleCloudFunctionFieldSet(PackageFieldSet):
    required_fields = (PythonGoogleCloudFunctionHandlerField,)

    handler: PythonGoogleCloudFunctionHandlerField
    runtime: PythonGoogleCloudFunctionRuntime
    complete_platforms: PythonFaaSCompletePlatforms
    pex3_venv_create_extra_args: PythonFaaSPex3VenvCreateExtraArgsField
    pex_build_extra_args: PythonFaaSPexBuildExtraArgs
    layout: PythonFaaSLayoutField
    type: PythonGoogleCloudFunctionType
    output_path: OutputPathField
    environment: EnvironmentField


@rule(desc="Create Python Google Cloud Function", level=LogLevel.DEBUG)
async def package_python_google_cloud_function(
    field_set: PythonGoogleCloudFunctionFieldSet,
) -> BuiltPackage:
    return await build_python_faas(
        BuildPythonFaaSRequest(
            address=field_set.address,
            target_name=PythonGoogleCloudFunction.alias,
            complete_platforms=field_set.complete_platforms,
            runtime=field_set.runtime,
            # GCF only supports x86_64 architecture for now.
            architecture=FaaSArchitecture.X86_64,
            handler=field_set.handler,
            pex3_venv_create_extra_args=field_set.pex3_venv_create_extra_args,
            pex_build_extra_args=field_set.pex_build_extra_args,
            layout=field_set.layout,
            output_path=field_set.output_path,
            include_requirements=True,
            include_sources=True,
            reexported_handler_module=PythonGoogleCloudFunctionHandlerField.reexported_handler_module,
            log_only_reexported_handler_func=True,
        )
    )


def rules():
    return [
        *collect_rules(),
        UnionRule(PackageFieldSet, PythonGoogleCloudFunctionFieldSet),
        *faas_rules(),
    ]
