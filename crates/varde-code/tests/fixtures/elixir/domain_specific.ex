defmodule AppRouter do
  @moduledoc "Phoenix-style router DSL exercising Route extraction."

  get "/users", UserController, :index
  post "/users", UserController, :create
  put "/users/:id", UserController, :update
  patch "/users/:id", UserController, :patch
  delete "/users/:id", UserController, :delete
end
