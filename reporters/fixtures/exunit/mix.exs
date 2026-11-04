defmodule GoodflowsExunitFixture.MixProject do
  use Mix.Project

  # Fixture suite for the goodflows ExUnit reporter drift guard. Fails on purpose.
  def project do
    [
      app: :goodflows_exunit_fixture,
      version: "0.0.0",
      elixir: "~> 1.18",
      deps: [{:goodflows_exunit_reporter, path: "../../exunit", only: :test}]
    ]
  end
end
