# Copyright 2020 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).
import json
from collections import defaultdict
from collections.abc import Iterable
from dataclasses import dataclass
from enum import Enum

from pants.engine.addresses import Address, Addresses
from pants.engine.collection import DeduplicatedCollection
from pants.engine.console import Console
from pants.engine.goal import Goal, GoalSubsystem, LineOriented
from pants.engine.internals.graph import resolve_dependencies
from pants.engine.rules import collect_rules, concurrently, goal_rule, implicitly, rule
from pants.engine.target import (
    AllUnexpandedTargets,
    AlwaysTraverseDeps,
    Dependencies,
    DependenciesRequest,
)
from pants.option.option_types import BoolOption, EnumOption
from pants.util.frozendict import FrozenDict
from pants.util.logging import LogLevel
from pants.util.ordered_set import FrozenOrderedSet


@dataclass(frozen=True)
class AddressToDependents:
    mapping: FrozenDict[Address, FrozenOrderedSet[Address]]


class DependentsOutputFormat(Enum):
    """Output format for listing dependents.

    text: List all dependents as a single list of targets in plain text.
    json: List all dependents as a mapping `{target: [dependents]}`.
    """

    text = "text"
    json = "json"


@rule(desc="Map all targets to their dependents", level=LogLevel.DEBUG)
async def map_addresses_to_dependents(all_targets: AllUnexpandedTargets) -> AddressToDependents:
    dependencies_per_target = await concurrently(
        resolve_dependencies(
            DependenciesRequest(
                tgt.get(Dependencies), should_traverse_deps_predicate=AlwaysTraverseDeps()
            ),
            **implicitly(),
        )
        for tgt in all_targets
    )

    address_to_dependents = defaultdict(set)
    for tgt, dependencies in zip(all_targets, dependencies_per_target):
        for dependency in dependencies:
            address_to_dependents[dependency].add(tgt.address)
    return AddressToDependents(
        FrozenDict(
            {
                addr: FrozenOrderedSet(dependents)
                for addr, dependents in address_to_dependents.items()
            }
        )
    )


@dataclass(frozen=True)
class DependentsRequest:
    addresses: FrozenOrderedSet[Address]
    transitive: bool
    include_roots: bool

    def __init__(
        self, addresses: Iterable[Address], *, transitive: bool, include_roots: bool
    ) -> None:
        object.__setattr__(self, "addresses", FrozenOrderedSet(addresses))
        object.__setattr__(self, "transitive", transitive)
        object.__setattr__(self, "include_roots", include_roots)


class Dependents(DeduplicatedCollection[Address]):
    sort_input = True


@rule(level=LogLevel.DEBUG)
async def find_dependents(
    request: DependentsRequest, address_to_dependents: AddressToDependents
) -> Dependents:
    check = set(request.addresses)
    known_dependents: set[Address] = set()
    while True:
        dependents = set(known_dependents)
        for target in check:
            target_dependents = address_to_dependents.mapping.get(target, FrozenOrderedSet())
            dependents.update(target_dependents)
        check = dependents - known_dependents
        if not check or not request.transitive:
            result = (
                dependents | set(request.addresses)
                if request.include_roots
                else dependents - set(request.addresses)
            )
            return Dependents(result)
        known_dependents = dependents


class DependentsSubsystem(LineOriented, GoalSubsystem):
    name = "dependents"
    help = "List all targets that depend on any of the input files/targets."

    transitive = BoolOption(
        default=False,
        help="List all transitive dependents. If unspecified, list direct dependents only.",
    )
    closed = BoolOption(
        default=False,
        help="Include the input targets in the output, along with the dependents.",
    )
    format = EnumOption(
        default=DependentsOutputFormat.text,
        help="Output format for listing dependents.",
    )


class DependentsGoal(Goal):
    subsystem_cls = DependentsSubsystem
    environment_behavior = Goal.EnvironmentBehavior.LOCAL_ONLY


async def list_dependents_as_plain_text(
    addresses: Addresses, dependents_subsystem: DependentsSubsystem, console: Console
) -> None:
    """Get dependents for given addresses and list them in the console as a single list."""
    dependents = await find_dependents(
        DependentsRequest(
            addresses,
            transitive=dependents_subsystem.transitive,
            include_roots=dependents_subsystem.closed,
        ),
        **implicitly(),
    )
    with dependents_subsystem.line_oriented(console) as print_stdout:
        for address in dependents:
            print_stdout(address.spec)


async def list_dependents_as_json(
    addresses: Addresses, dependents_subsystem: DependentsSubsystem, console: Console
) -> None:
    """Get dependents for given addresses and list them in the console in JSON."""
    dependents_group = await concurrently(
        find_dependents(
            DependentsRequest(
                (address,),
                transitive=dependents_subsystem.transitive,
                include_roots=dependents_subsystem.closed,
            ),
            **implicitly(),
        )
        for address in addresses
    )
    iterated_addresses = []
    for dependents in dependents_group:
        iterated_addresses.append(sorted([str(address) for address in dependents]))
    mapping = dict(zip([str(address) for address in addresses], iterated_addresses))
    output = json.dumps(mapping, indent=4)
    with dependents_subsystem.line_oriented(console) as print_stdout:
        print_stdout(output)


@goal_rule
async def dependents_goal(
    specified_addresses: Addresses, dependents_subsystem: DependentsSubsystem, console: Console
) -> DependentsGoal:
    if DependentsOutputFormat.text == dependents_subsystem.format:
        await list_dependents_as_plain_text(
            addresses=specified_addresses,
            dependents_subsystem=dependents_subsystem,
            console=console,
        )
    elif DependentsOutputFormat.json == dependents_subsystem.format:
        await list_dependents_as_json(
            addresses=specified_addresses,
            dependents_subsystem=dependents_subsystem,
            console=console,
        )
    return DependentsGoal(exit_code=0)


def rules():
    return collect_rules()
