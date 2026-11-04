# A tag filter, so the report carries `filters` and one test is excluded.
ExUnit.start(exclude: [:slow])
Goodflows.ExUnitReporter.configure()
