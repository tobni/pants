# Copyright 2020 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).

from __future__ import annotations

from pants.engine.internals.native_engine import AncestorFiles as AncestorFiles  # noqa: F401
from pants.engine.internals.native_engine import (  # noqa: F401
    AncestorFilesRequest as AncestorFilesRequest,
)
from pants.engine.internals.native_engine import (  # noqa: F401
    find_ancestor_files as find_ancestor_files,
)


def rules():
    return []
