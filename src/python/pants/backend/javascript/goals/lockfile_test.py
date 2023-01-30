# Copyright 2022 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).
import json

import pytest

from pants.backend.javascript.goals import lockfile
from pants.backend.javascript.goals.lockfile import (
    GeneratePackageLockJsonFile,
    KnownPackageJsonUserResolveNamesRequest,
)
from pants.backend.javascript.package_json import (
    AllPackageJson,
    PackageJson,
    PackageJsonSourceField,
    PackageJsonTarget,
    ReadPackageJsonRequest,
)
from pants.build_graph.address import Address
from pants.core.goals.generate_lockfiles import GenerateLockfileResult, KnownUserResolveNames
from pants.engine.fs import DigestContents
from pants.engine.rules import QueryRule
from pants.testutil.rule_runner import RuleRunner


@pytest.fixture
def rule_runner() -> RuleRunner:
    return RuleRunner(
        rules=[
            *lockfile.rules(),
            QueryRule(
                KnownUserResolveNames, (KnownPackageJsonUserResolveNamesRequest, AllPackageJson)
            ),
            QueryRule(AllPackageJson, ()),
            QueryRule(GenerateLockfileResult, (GeneratePackageLockJsonFile,)),
            QueryRule(PackageJson, (ReadPackageJsonRequest,)),
        ],
        target_types=[PackageJsonTarget],
    )


def given_package_with_name(name: str) -> str:
    return json.dumps({"name": name, "version": "0.0.1"})


def test_resolves_are_package_names(rule_runner: RuleRunner) -> None:
    rule_runner.write_files(
        {
            "src/js/foo/BUILD": "package_json()",
            "src/js/foo/package.json": given_package_with_name("ham"),
            "src/js/bar/BUILD": "package_json()",
            "src/js/bar/package.json": given_package_with_name("spam"),
        }
    )
    pkg_jsons = rule_runner.request(AllPackageJson, [])
    resolves = rule_runner.request(
        KnownUserResolveNames, (pkg_jsons, KnownPackageJsonUserResolveNamesRequest())
    )
    assert set(resolves.names) == {"ham", "spam"}


def test_generates_lockfile_for_package_json(rule_runner: RuleRunner) -> None:
    rule_runner.write_files(
        {
            "src/js/BUILD": "package_json()",
            "src/js/package.json": given_package_with_name("ham"),
        }
    )
    tgt = rule_runner.get_target(Address("src/js"))
    pkg_json = rule_runner.request(
        PackageJson, [ReadPackageJsonRequest(tgt[PackageJsonSourceField])]
    )

    lockfile = rule_runner.request(
        GenerateLockfileResult,
        (
            GeneratePackageLockJsonFile(
                resolve_name="ham",
                lockfile_dest="src/js/package-lock.json",
                pkg_json=pkg_json,
                diff=False,
            ),
        ),
    )

    digest_contents = rule_runner.request(DigestContents, [lockfile.digest])

    assert json.loads(digest_contents[0].content) == {
        "name": "ham",
        "version": "0.0.1",
        "lockfileVersion": 2,
        "requires": True,
        "packages": {"": {"name": "ham", "version": "0.0.1"}},
    }
