# Copyright 2022 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).

from __future__ import annotations

import logging
from abc import ABCMeta
from collections.abc import Iterable
from dataclasses import dataclass
from itertools import chain
from typing import cast

from pants.core.goals.package import PackageFieldSet
from pants.core.goals.publish import (
    PublishFieldSet,
    PublishProcesses,
    PublishProcessesRequest,
    package_for_publish,
)
from pants.engine.console import Console
from pants.engine.environment import ChosenLocalEnvironmentName, EnvironmentName
from pants.engine.goal import Goal, GoalSubsystem
from pants.engine.internals.graph import find_valid_field_sets
from pants.engine.internals.specs_rules import find_valid_field_sets_for_target_roots
from pants.engine.intrinsics import execute_process, run_interactive_process
from pants.engine.process import (
    FallibleProcessResult,
    InteractiveProcess,
    InteractiveProcessResult,
    Process,
)
from pants.engine.rules import Get, collect_rules, concurrently, goal_rule, implicitly, rule
from pants.engine.target import (
    FieldSet,
    FieldSetsPerTargetRequest,
    NoApplicableTargetsBehavior,
    Target,
    TargetRootsToFieldSetsRequest,
)
from pants.engine.unions import union
from pants.option.option_types import BoolOption
from pants.util.strutil import pluralize, softwrap

logger = logging.getLogger(__name__)


@union(in_scope_types=[EnvironmentName])
@dataclass(frozen=True)
class DeployFieldSet(FieldSet, metaclass=ABCMeta):
    """The FieldSet type for the `deploy` goal.

    Union members may list any fields required to fulfill the instantiation of the `DeployProcess`
    result of the deploy rule.
    """


@dataclass(frozen=True)
class DeployProcess:
    """A process that when executed will have the side effect of deploying a target.

    To provide with the ability to deploy a given target, create a custom `DeployFieldSet` for
    that given target and implement a rule that returns `DeployProcess` for that custom field set:

    Example:

        @dataclass(frozen=True)
        class MyDeploymentFieldSet(DeployFieldSet):
            pass

        @rule
        async def my_deployment_process(field_set: MyDeploymentFieldSet) -> DeployProcess:
            # Create the underlying process that executes the deployment
            process = Process(...)
            return DeployProcess(
                name="my_deployment",
                process=InteractiveProcess.from_process(process)
            )

        def rules():
            return [
                *collect_rules(),
                UnionRule(DeployFieldSet, MyDeploymentFieldSet)
            ]

    Use the `publish_dependencies` field to provide with a list of targets that produce packages
    which need to be externally published before the deployment process is executed.
    """

    name: str
    process: InteractiveProcess | None
    publish_dependencies: tuple[Target, ...] = ()
    description: str | None = None


class DeploySubsystem(GoalSubsystem):
    name = "experimental-deploy"
    help = "Perform a deployment process."

    dry_run = BoolOption(
        default=False,
        help=softwrap(
            """
            If true, perform a dry run without deploying anything.
            For example, when deploying a terraform_deployment, a plan will be executed instead of an apply.
            """
        ),
    )
    publish_dependencies = BoolOption(
        default=True,
        help=softwrap(
            """
            If false, don't publish target dependencies before deploying the target.
            For example, when deploying a helm_deployment, dependent docker images will not be published.
            """
        ),
    )

    required_union_implementation = (DeployFieldSet,)


@dataclass(frozen=True)
class Deploy(Goal):
    subsystem_cls = DeploySubsystem
    environment_behavior = Goal.EnvironmentBehavior.LOCAL_ONLY  # TODO(#17129) — Migrate this.


@dataclass(frozen=True)
class _PublishProcessesForTargetRequest:
    target: Target


@rule
async def publish_process_for_target(
    request: _PublishProcessesForTargetRequest,
) -> PublishProcesses:
    package_field_sets, publish_field_sets = await concurrently(
        find_valid_field_sets(
            FieldSetsPerTargetRequest(PackageFieldSet, [request.target]), **implicitly()
        ),
        find_valid_field_sets(
            FieldSetsPerTargetRequest(PublishFieldSet, [request.target]), **implicitly()
        ),
    )

    return await package_for_publish(
        PublishProcessesRequest(
            package_field_sets=package_field_sets.field_sets,
            publish_field_sets=publish_field_sets.field_sets,
        ),
        **implicitly(),
    )


async def _all_publish_processes(targets: Iterable[Target]) -> PublishProcesses:
    processes_per_target = await concurrently(
        publish_process_for_target(_PublishProcessesForTargetRequest(target)) for target in targets
    )

    return PublishProcesses(chain.from_iterable(processes_per_target))


def _process_results_to_string(
    console: Console,
    res: InteractiveProcessResult | FallibleProcessResult,
    *,
    names: Iterable[str],
    success_status: str,
    description: str | None = None,
) -> tuple[int, tuple[str, ...]]:
    results = []
    if res.exit_code == 0:
        sigil = console.sigil_succeeded()
        status = success_status
        prep = "to"
    else:
        sigil = console.sigil_failed()
        status = "failed"
        prep = "for"

    if description:
        status += f" {prep} {description}"

    for name in names:
        results.append(f"{sigil} {name} {status}")
    return res.exit_code, tuple(results)


async def _invoke_process(
    console: Console,
    process: InteractiveProcess | None,
    *,
    names: Iterable[str],
    success_status: str,
    description: str | None = None,
) -> tuple[int, tuple[str, ...]]:
    results = []

    if not process:
        sigil = console.sigil_skipped()
        status = "skipped"
        if description:
            status += f" {description}"
        for name in names:
            results.append(f"{sigil} {name} {status}.")
        return 0, tuple(results)

    logger.debug(f"Execute {process}")
    res = await run_interactive_process(process)
    return _process_results_to_string(
        console, res, names=names, success_status=success_status, description=description
    )


@goal_rule
async def run_deploy(
    console: Console,
    deploy_subsystem: DeploySubsystem,
    local_environment: ChosenLocalEnvironmentName,
) -> Deploy:
    target_roots_to_deploy_field_sets = await find_valid_field_sets_for_target_roots(
        TargetRootsToFieldSetsRequest(
            DeployFieldSet,
            goal_description=f"the `{deploy_subsystem.name}` goal",
            no_applicable_targets_behavior=NoApplicableTargetsBehavior.error,
        ),
        **implicitly(),
    )

    deploy_processes = await concurrently(
        Get(DeployProcess, DeployFieldSet, field_set)
        for field_set in target_roots_to_deploy_field_sets.field_sets
    )

    publish_targets = (
        set(chain.from_iterable([deploy.publish_dependencies for deploy in deploy_processes]))
        if deploy_subsystem.publish_dependencies
        else set()
    )

    logger.debug(f"Found {pluralize(len(publish_targets), 'dependency')}")
    publish_processes = await _all_publish_processes(publish_targets)

    exit_code: int = 0
    results: list[str] = []

    if publish_processes:
        logger.info(f"Publishing {pluralize(len(publish_processes), 'dependency')}...")
        background_publish_processes = [
            publish for publish in publish_processes if isinstance(publish.process, Process)
        ]
        foreground_publish_processes = [
            publish
            for publish in publish_processes
            if isinstance(publish.process, InteractiveProcess) or publish.process is None
        ]

        # Publish all background deployments first
        background_results = await concurrently(
            execute_process(
                **implicitly(
                    {
                        cast(Process, publish.process): Process,
                        local_environment.val: EnvironmentName,
                    }
                )
            )
            for publish in background_publish_processes
        )
        for pub, res in zip(background_publish_processes, background_results):
            ec, statuses = _process_results_to_string(
                console,
                res,
                names=pub.names,
                description=pub.description,
                success_status="published",
            )
            exit_code = ec if ec != 0 else exit_code
            results.extend(statuses)

        # Publish all foreground deployments next.
        for publish in foreground_publish_processes:
            process = cast(InteractiveProcess | None, publish.process)
            ec, statuses = await _invoke_process(
                console,
                process,
                names=publish.names,
                description=publish.description,
                success_status="published",
            )
            exit_code = ec if ec != 0 else exit_code
            results.extend(statuses)

    # Only proceed to deploy of all dependencies have been successfully published
    if exit_code == 0 and deploy_processes:
        logger.info("Deploying targets...")

        for deploy in deploy_processes:
            # Invoke the deployment.
            ec, statuses = await _invoke_process(
                console,
                deploy.process,
                names=[deploy.name],
                success_status="deployed",
                description=deploy.description,
            )
            exit_code = ec if ec != 0 else exit_code
            results.extend(statuses)

    console.print_stderr("")
    if not results:
        sigil = console.sigil_skipped()
        console.print_stderr(f"{sigil} Nothing deployed.")

    for line in results:
        console.print_stderr(line)

    return Deploy(exit_code)


def rules():
    return collect_rules()
