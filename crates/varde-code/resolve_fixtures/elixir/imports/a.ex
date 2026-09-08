defmodule A do
  # `alias Helper` — the import spec's trailing segment `Helper` matches the
  # sibling file stem `Helper.ex`, exercising a genuinely RESOLVED import edge
  # through resolve.rs's shared stem-matching. Elixir imports are module-based
  # and in general do NOT stem-match files; this fixture is deliberately
  # crafted (module name == sibling file stem) so the edge resolves.
  alias Helper

  def run do
    Helper.work()
  end
end
