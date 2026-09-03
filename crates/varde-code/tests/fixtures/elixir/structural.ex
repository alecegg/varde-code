defmodule Greeter do
  @moduledoc "A greeting module."
  @default_name "world"

  defstruct name: nil, count: 0

  def greet(name) do
    prefix = "Hello, "
    prefix <> name
  end

  defp helper(value), do: value

  defmacro shout(msg) do
    msg
  end
end

defprotocol Sizeable do
  def size(data)
end

defimpl Sizeable, for: BitString do
  def size(s), do: byte_size(s)
end
