defmodule Expressions do
  import Enum
  alias String.Chars
  require Logger
  use GenServer

  def run(input) do
    total = 1
    {first, second} = {2, 3}
    label = :ok
    flag = true
    nothing = nil
    ratio = 1.5
    greeting = "hi"

    Enum.map(input, fn x -> x end)
    local_call(total)
    input.field
  end

  def local_call(x) do
    x
  end
end
