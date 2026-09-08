defmodule MyApp.User do
  # `alias MyApp.Repo` names the module by its PascalCase path, which does NOT
  # stem-match the snake_case file `repo.ex`; it resolves only via the
  # module-declaration index.
  alias MyApp.Repo

  def fetch(id) do
    Repo.get(id)
  end
end
