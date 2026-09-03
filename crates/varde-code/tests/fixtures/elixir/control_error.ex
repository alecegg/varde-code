defmodule ControlError do
  def classify(x) do
    if x > 0, do: :positive, else: :non_positive

    unless x == 0, do: :nonzero

    case x do
      0 -> :zero
      _ -> :other
    end

    cond do
      x > 10 -> :big
      true -> :small
    end

    for i <- [1, 2, 3], do: i

    with {:ok, v} <- {:ok, x}, do: v

    receive do
      msg -> msg
    end
  end

  def risky(x) do
    try do
      raise ArgumentError, "boom"
    rescue
      e in RuntimeError -> e
    catch
      :exit, v -> v
    end

    throw(:aborted)
    x
  end
end
