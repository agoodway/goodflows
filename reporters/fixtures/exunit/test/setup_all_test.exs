defmodule Fixture.SetupAllTest do
  use ExUnit.Case

  # `setup_all` raises, so every test here and the group are errored.
  setup_all do
    raise "fixture setup_all fails"
  end

  test "never runs" do
    assert true
  end
end
