# Re-export `Thing` at the package root so consumers can `from pkg import Thing`
# without knowing it lives in the `core` submodule.
from .core import Thing
