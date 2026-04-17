# Copyright 2020 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).

from __future__ import annotations

import os

from pants.engine.internals.native_engine import AncestorFiles as AncestorFiles  # noqa: F401
from pants.engine.internals.native_engine import (  # noqa: F401
    AncestorFilesRequest as AncestorFilesRequest,
)
from pants.engine.internals.native_engine import find_ancestor_files as _native_find_ancestor_files
from pants.engine.rules import collect_rules, rule


def putative_ancestor_files(input_files: tuple[str, ...], requested: tuple[str, ...]) -> set[str]:
    """Return the paths of potentially missing ancestor files.

    NB: The sources are expected to not have had their source roots stripped.
    Therefore this function will consider superfluous files at and above the source roots,
    (e.g., src/python/<name>, src/<name>). It is the caller's responsibility to filter these
    out if necessary.
    """
    packages: set[str] = set()
    for input_file in input_files:
        if not input_file.endswith((".py", ".pyi")):
            continue
        pkg_dir = os.path.dirname(input_file)
        if pkg_dir in packages:
            continue
        package = ""
        packages.add(package)
        for component in pkg_dir.split(os.sep):
            package = os.path.join(package, component)
            packages.add(package)

    return {
        os.path.join(package, requested_f) for package in packages for requested_f in requested
    } - set(input_files)


@rule
async def find_ancestor_files(request: AncestorFilesRequest) -> AncestorFiles:
    return await _native_find_ancestor_files(request)


def rules():
    return collect_rules()
