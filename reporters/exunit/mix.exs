defmodule Goodflows.ExUnitReporter.MixProject do
  use Mix.Project

  # Versioned on its own, apart from the goodflows CLI; released by tagging
  # exunit-reporter-v<version> (see docs/releasing.md).
  @version "0.6.0"

  def project do
    [
      app: :goodflows_exunit_reporter,
      version: @version,
      elixir: "~> 1.18",
      start_permanent: false,
      deps: [],
      description: "ExUnit formatter that writes goodflows test-timings/1 reports",
      source_url: "https://github.com/agoodway/goodflows-exunit-reporter",
      package: package()
    ]
  end

  defp package do
    [
      licenses: ["MIT"],
      links: %{
        "GitHub" => "https://github.com/agoodway/goodflows-exunit-reporter",
        "goodflows" => "https://github.com/agoodway/goodflows"
      },
      files: ~w(lib mix.exs README.md LICENSE)
    ]
  end

  def application do
    [extra_applications: [:ex_unit]]
  end
end
