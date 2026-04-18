# Copyright 2020 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).

from __future__ import annotations

import pytest

from pants.backend.python.util_rules import ancestor_files
from pants.backend.python.util_rules.ancestor_files import (
    AncestorFiles,
    AncestorFilesRequest,
)
from pants.testutil.rule_runner import QueryRule, RuleRunner


@pytest.fixture
def rule_runner() -> RuleRunner:
    return RuleRunner(
        rules=[
            *ancestor_files.rules(),
            QueryRule(AncestorFiles, (AncestorFilesRequest,)),
        ]
    )


def assert_injected(
    rule_runner: RuleRunner,
    *,
    input_files: list[str],
    empty_files: list[str],
    nonempty_files: list[str],
    expected_discovered: list[str],
    ignore_empty_files: bool,
) -> None:
    rule_runner.write_files(
        {**dict.fromkeys(empty_files, ""), **dict.fromkeys(nonempty_files, "foo")}
    )
    request = AncestorFilesRequest(
        requested=("__init__.py",),
        input_files=tuple(input_files),
        ignore_empty_files=ignore_empty_files,
    )
    result = rule_runner.request(AncestorFiles, [request]).snapshot
    assert list(result.files) == sorted(expected_discovered)


@pytest.mark.parametrize("ignore_empty_files", [False, True])
def test_rule(rule_runner: RuleRunner, ignore_empty_files: bool) -> None:
    assert_injected(
        rule_runner,
        input_files=[
            "src/python/project/lib.py",
            "src/python/project/subdir/__init__.py",
            "src/python/project/subdir/lib.py",
            "src/python/no_init/lib.py",
        ],
        nonempty_files=[
            "src/python/__init__.py",
            "tests/python/project/__init__.py",
        ],
        empty_files=["src/python/project/__init__.py"],
        ignore_empty_files=ignore_empty_files,
        expected_discovered=(
            ["src/python/__init__.py"]
            + ([] if ignore_empty_files else ["src/python/project/__init__.py"])
        ),
    )


