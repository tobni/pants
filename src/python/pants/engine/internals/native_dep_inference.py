# Copyright 2023 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).

from __future__ import annotations

from dataclasses import dataclass
from typing import Iterable

from pants.engine.internals.native_engine import NativeDependenciesRequest
from pants.engine.rules import QueryRule
from pants.util.frozendict import FrozenDict


@dataclass(frozen=True)
class NativeParsedPythonDependencies:
    imports: FrozenDict[str, tuple[int, bool]]
    string_candidates: FrozenDict[str, int]

    def __init__(self, imports: dict[str, tuple[int, bool]], string_candidates: dict[str, int]):
        object.__setattr__(self, "imports", FrozenDict(imports))
        object.__setattr__(self, "string_candidates", FrozenDict(string_candidates))


def rules() -> Iterable[QueryRule]:
    # Keep in sync with `intrinsics.rs`.
    return (QueryRule(NativeParsedPythonDependencies, (NativeDependenciesRequest,)),)
