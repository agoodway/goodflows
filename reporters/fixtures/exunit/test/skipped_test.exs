defmodule Fixture.SkippedTest do
  use ExUnit.Case, async: true

  # Every test here is skipped, so the group is skipped with only skipped counts.
  @tag :skip
  test "is skipped first" do
    assert false
  end

  @tag :skip
  test "is skipped second" do
    assert false
  end
end
