from .core.runtime import require_supported_python

# Before any route or dependency loads: an untested interpreter is refused
# with its reason rather than failing later inside a dependency.
require_supported_python()
