defmodule Fixture.OutcomesTest do
  use ExUnit.Case, async: true

  test "passes" do
    assert 1 + 1 == 2
  end

  test "fails on purpose" do
    assert "private assertion value" == "another value"
  end

  @tag :skip
  test "is skipped" do
    assert false
  end

  @tag :slow
  test "is excluded by the tag filter" do
    assert true
  end

  # A test without a body is ExUnit's todo: it is reported as not implemented.
  test "is a todo"

  describe "math" do
    test "adds" do
      assert 1 + 2 == 3
    end

    test "multiplies" do
      assert 2 * 3 == 6
    end
  end
end
