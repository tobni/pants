# Copyright 2021 Pants project contributors (see CONTRIBUTORS.md).
# Licensed under the Apache License, Version 2.0 (see LICENSE).

from __future__ import annotations

# NB: This class is re-exported in pants.engine.fs as part of the public Plugin API.
#   Backend code in the Pants repo should import this class from there, to model idiomatic
#   use of that API. However this class is also used by code in base, core, and options, which
#   must not depend on pants.engine.fs, so those must import directly from here.
from pants.engine.internals.native_engine import (  # noqa: F401
    GlobMatchErrorBehavior as GlobMatchErrorBehavior,
)
