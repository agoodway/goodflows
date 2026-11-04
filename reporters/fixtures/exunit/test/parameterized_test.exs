defmodule Fixture.ParameterizedTest do
  use ExUnit.Case, async: true, parameterize: [%{mode: :a}, %{mode: :b}]

  test "runs once per parameter", %{mode: mode} do
    assert mode in [:a, :b]
  end
end
