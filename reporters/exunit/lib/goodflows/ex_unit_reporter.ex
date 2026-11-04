defmodule Goodflows.ExUnitReporter do
  @moduledoc """
  Streams structural ExUnit timing data to `test-timings/1` JSON Lines reports
  that goodflows collects.

  Add it next to the formatters you already use, after `ExUnit.start/1`:

      ExUnit.start()
      Goodflows.ExUnitReporter.configure()

  Each run is written to a new `<report_id>.jsonl` file in ExUnit's
  `:test_timing_directory` option when set, otherwise in a nonempty
  `GOODFLOWS_REPORT_DIR`, otherwise in `_build/test/timings/`. When the option
  and `GOODFLOWS_REPORT_DIR` name different directories, the option wins and one
  warning says goodflows will not collect the report. The suite identity is the
  first of the `:test_timing_suite_id` option, `TEST_TIMING_SUITE_ID` and
  `GOODFLOWS_RUN_ID` that follows the identifier grammar, otherwise the report
  id.

  `suite_started.meta` carries `reporter` (`"goodflows_exunit_reporter"`),
  `reporter_version`, and `filtered`: `true` when the run selected tests by tag
  or name (nonempty `:include`/`:exclude`, or `:only_test_ids`), else `false`.

  Durations and offsets are integer microseconds. Only structural data is
  written: never captured output, logs, failure messages, parameter values or
  arbitrary tags. A reporter failure, including an unwritable directory, prints
  one warning and stops writing for that run; it never changes test results.
  """
  use GenServer

  @format "test-timings/1"
  @max_safe_integer 9_007_199_254_740_991
  @max_text_bytes 4_096
  @max_filters 256
  @reporter "goodflows_exunit_reporter"
  # Read while this package compiles, as a dependency or on its own.
  @reporter_version Mix.Project.config()[:version]

  @doc "Appends the reporter to the configured formatters, keeping the others."
  def configure do
    formatters = ExUnit.configuration()[:formatters]
    ExUnit.configure(formatters: Enum.uniq(List.wrap(formatters) ++ [__MODULE__]))
  end

  @doc false
  def start_link(opts), do: GenServer.start_link(__MODULE__, opts)

  @impl true
  def init(opts) do
    goodflows_directory = present_env("GOODFLOWS_REPORT_DIR")
    directory = opts[:test_timing_directory] || goodflows_directory || "_build/test/timings"
    warn_uncollected(opts[:test_timing_directory], goodflows_directory)

    report_id =
      "#{System.system_time(:microsecond)}-#{System.pid()}-#{System.unique_integer([:positive])}"

    suite_id =
      identifier(opts[:test_timing_suite_id]) || identifier(present_env("TEST_TIMING_SUITE_ID")) ||
        identifier(present_env("GOODFLOWS_RUN_ID")) || report_id

    state = %{
      output: nil,
      suite_id: suite_id,
      report_id: report_id,
      origin: System.monotonic_time(:microsecond),
      next_id: 1,
      groups: %{},
      tests: %{},
      counts: %{},
      skips: %{},
      aborted: false
    }

    path = Path.join(directory, report_id <> ".jsonl")

    with :ok <- File.mkdir_p(directory),
         {:ok, output} <- File.open(path, [:write, :binary, :exclusive]) do
      {:ok, %{state | output: output}}
    else
      {:error, reason} -> {:ok, disable(state, "cannot open #{path}: #{inspect(reason)}")}
    end
  end

  @impl true
  def handle_cast(_event, %{output: nil} = state), do: {:noreply, state}

  def handle_cast(event, state) do
    {:noreply, handle(event, state)}
  rescue
    exception -> {:noreply, disable(state, Exception.message(exception))}
  catch
    kind, reason -> {:noreply, disable(state, Exception.format_banner(kind, reason))}
  end

  @impl true
  def terminate(_reason, %{output: nil}), do: :ok
  def terminate(_reason, state), do: File.close(state.output)

  defp handle({:suite_started, opts}, state) do
    state = %{state | origin: System.monotonic_time(:microsecond)}
    {partition, partition_count} = partition()

    write(state, %{
      event: "suite_started",
      format: @format,
      started_at: DateTime.to_iso8601(DateTime.utc_now()),
      runner: %{name: "exunit", version: System.version()},
      partition: partition,
      partition_count: partition_count,
      seed: integer(opts[:seed]),
      filters: %{include: filters(opts[:include]), exclude: filters(opts[:exclude])},
      meta: %{
        elixir_version: System.version(),
        otp_release: System.otp_release(),
        max_cases: integer(opts[:max_cases]),
        schedulers: System.schedulers_online(),
        reporter: @reporter,
        reporter_version: @reporter_version,
        filtered: filtered?(opts)
      }
    })
  end

  defp handle({:module_started, test_module}, state) do
    parameter_id = parameter_id(test_module.parameters)
    {started, state} = start(state, :groups, group_key(test_module), group_fields(test_module))

    write(state, %{
      started
      | event: "group_started",
        meta: group_meta(test_module, parameter_id)
    })
  end

  defp handle({:module_finished, test_module}, state) do
    parameter_id = parameter_id(test_module.parameters)

    # ExUnit's runner always starts a module before finishing it, but a finish
    # without an open start (for example one delivered by a custom runner) still
    # gets a start written first, as tests do, so the pair matches.
    state =
      if Map.get(state.groups, group_key(test_module), []) == [],
        do: handle({:module_started, test_module}, state),
        else: state

    {started, state} = take(state, :groups, group_key(test_module))

    # `test_module.tests` holds the run and invalid tests but leaves out the
    # excluded and skipped ones, which are counted as they finish.
    {skipped, state} = take_skips(state, test_module)
    counts = Enum.reduce(test_module.tests, %{}, &increment(&2, outcome(&1.state)))

    counts =
      if skipped > 0, do: Map.update(counts, "skipped", skipped, &(&1 + skipped)), else: counts

    finished =
      Map.merge(started, %{
        event: "group_finished",
        meta: group_meta(test_module, parameter_id),
        duration_us: elapsed(state, started),
        outcome: group_outcome(test_module, counts),
        counts: counts
      })

    write(state, finished)
  end

  defp handle({:test_started, test}, state) do
    {started, state} = start(state, :tests, test_key(test), test_fields(test))
    write(state, %{started | event: "test_started", meta: test_meta(test)})
  end

  defp handle({:test_finished, test}, state) do
    # ExUnit does not deliver a start for every test it reports (for example
    # tests made invalid by a failing `setup_all`), so a start is written
    # first when none is open; the pair then matches by construction.
    state =
      if Map.get(state.tests, test_key(test), []) == [],
        do: handle({:test_started, test}, state),
        else: state

    {started, state} = take(state, :tests, test_key(test))
    outcome = outcome(test.state)
    duration = if outcome in ["passed", "failed"], do: integer(test.time)
    meta = Map.merge(test_meta(test), skip_reason(test.state))

    finished =
      Map.merge(started, %{
        event: "test_finished",
        meta: meta,
        duration_us: duration,
        outcome: outcome
      })

    state = write(state, finished)
    state = %{state | counts: increment(state.counts, outcome)}

    if skip_reason(test.state) == %{},
      do: state,
      else: %{state | skips: put_skip(state.skips, test)}
  end

  defp handle({:suite_finished, times}, state) do
    run = integer(times[:run])
    async = integer(times[:async])
    load = integer(times[:load])

    write(state, %{
      event: "suite_finished",
      aborted: state.aborted,
      unfinished_tests: open_count(state.tests),
      unfinished_groups: open_count(state.groups),
      counts: state.counts,
      duration_us: run,
      meta: %{
        run_us: run,
        async_us: async,
        sync_us: if(run, do: max(run - (async || 0), 0)),
        load_us: load,
        total_us: if(run, do: run + (load || 0))
      }
    })
  end

  defp handle(:max_failures_reached, state) do
    %{write(state, %{event: "max_failures_reached"}) | aborted: true}
  end

  defp handle({:sigquit, _running}, state) do
    %{write(state, %{event: "interrupted"}) | aborted: true}
  end

  defp handle(_event, state), do: state

  # Each record is one complete line, written before the next event is handled.
  defp write(state, record) do
    record =
      Map.merge(record, %{
        suite_id: state.suite_id,
        report_id: state.report_id,
        received_offset_us: offset(state)
      })

    case :file.write(state.output, [JSON.encode_to_iodata!(record), "\n"]) do
      :ok -> state
      {:error, reason} -> disable(state, "cannot write the report: #{inspect(reason)}")
    end
  end

  # The option still wins; the warning only says goodflows will not see the file.
  defp warn_uncollected(directory, goodflows_directory)
       when is_binary(directory) and is_binary(goodflows_directory) do
    if Path.expand(directory) != Path.expand(goodflows_directory) do
      IO.puts(
        :stderr,
        "warning: Goodflows.ExUnitReporter writes to #{directory} (:test_timing_directory), " <>
          "not GOODFLOWS_REPORT_DIR (#{goodflows_directory}), so goodflows will not collect this report"
      )
    end
  end

  defp warn_uncollected(_directory, _goodflows_directory), do: :ok

  defp disable(state, reason) do
    IO.puts(:stderr, "warning: Goodflows.ExUnitReporter stopped writing: #{reason}")
    if state.output, do: File.close(state.output)
    %{state | output: nil}
  end

  defp offset(state), do: max(System.monotonic_time(:microsecond) - state.origin, 0)

  defp elapsed(state, started), do: max(offset(state) - started.started_offset_us, 0)

  # Finish events reuse the stored start fields, so each pair matches by construction.
  defp start(state, kind, key, fields) do
    started =
      Map.merge(fields, %{
        event: nil,
        meta: nil,
        execution_id: state.next_id,
        started_offset_us: offset(state)
      })

    open = Map.update(Map.fetch!(state, kind), key, [started], &(&1 ++ [started]))
    {started, %{state | kind => open, next_id: state.next_id + 1}}
  end

  defp take(state, kind, key) do
    case Map.get(Map.fetch!(state, kind), key, []) do
      [] -> {nil, state}
      [first] -> {first, Map.update!(state, kind, &Map.delete(&1, key))}
      [first | rest] -> {first, Map.update!(state, kind, &Map.put(&1, key, rest))}
    end
  end

  defp put_skip(skips, test) do
    Map.update(
      skips,
      {test.module, test.parameters},
      MapSet.new([test.name]),
      &MapSet.put(&1, test.name)
    )
  end

  # ExUnit gives only the tests it runs their group's parameters, so in a
  # parameterized module the excluded and skipped tests arrive with none. Each
  # parameter group skips the same tests (filters never see parameters) and
  # reports them before it finishes, so that shared set counts for every group
  # and is dropped once none of the module's groups is open.
  defp take_skips(state, %{name: module, parameters: parameters}) do
    {own, skips} = Map.pop(state.skips, {module, parameters}, MapSet.new())

    {shared, skips} =
      cond do
        parameters == %{} -> {MapSet.new(), skips}
        open_group?(state, module) -> {Map.get(skips, {module, %{}}, MapSet.new()), skips}
        true -> Map.pop(skips, {module, %{}}, MapSet.new())
      end

    {MapSet.size(MapSet.union(own, shared)), %{state | skips: skips}}
  end

  defp open_group?(state, module),
    do:
      Enum.any?(state.groups, fn {{name, _parameters}, open} -> name == module and open != [] end)

  defp open_count(open),
    do: Enum.sum(Enum.map(open, fn {_key, executions} -> length(executions) end))

  defp group_key(test_module), do: {test_module.name, test_module.parameters}
  defp test_key(test), do: {test.module, test.name, test.parameters}

  # A hash of the parameters, so the same parameters get the same id in every
  # run and partition without their values being written.
  defp parameter_id(parameters) when map_size(parameters) == 0, do: nil
  defp parameter_id(parameters), do: :erlang.phash2(parameters, 4_294_967_296)

  defp test_fields(test) do
    %{
      group: text(inspect(test.module)),
      path: path(test.tags[:describe]),
      name: text(test_name(test.name, test.tags[:describe])),
      file: relative_file(test.tags[:file]),
      line: line(test.tags[:line])
    }
  end

  # ExUnit names a test "test <describe> <title>"; the title alone is `name`.
  # Other names (for example doctests) are kept whole.
  defp test_name(name, describe) do
    case Atom.to_string(name) do
      "test " <> title when is_binary(describe) and describe != "" ->
        String.replace_prefix(title, describe <> " ", "")

      "test " <> title ->
        title

      name ->
        name
    end
  end

  # Excluded and skipped tests never get the runner's `async` tag, so the
  # module's setting stands in for it.
  defp test_meta(test) do
    async = Map.get_lazy(test.tags, :async, fn -> module_async(test.module) end)
    %{async: boolean(async), parameter_id: parameter_id(test.parameters)}
  end

  defp group_fields(test_module) do
    %{group: text(inspect(test_module.name)), file: relative_file(test_module.file)}
  end

  defp group_meta(test_module, parameter_id) do
    %{
      async: module_async(test_module.name),
      setup_all: test_module.setup_all?,
      parameter_id: parameter_id
    }
  end

  defp module_async(module) do
    if Code.ensure_loaded?(module) and function_exported?(module, :__ex_unit__, 1) do
      boolean(module.__ex_unit__(:config)[:async?])
    end
  end

  defp group_outcome(test_module, counts) do
    cond do
      match?({:failed, _}, test_module.state) -> "errored"
      Map.has_key?(counts, "errored") -> "errored"
      Map.has_key?(counts, "failed") -> "failed"
      counts != %{} and Map.keys(counts) == ["skipped"] -> "skipped"
      true -> "passed"
    end
  end

  # `:only_test_ids` is how Mix runs selected tests (for example `--failed`).
  defp filtered?(opts) do
    List.wrap(opts[:include]) != [] or List.wrap(opts[:exclude]) != [] or
      selected?(opts[:only_test_ids])
  end

  defp selected?(ids) when is_list(ids), do: ids != []
  defp selected?(%MapSet{} = ids), do: MapSet.size(ids) > 0
  defp selected?(_ids), do: false

  defp outcome(nil), do: "passed"
  defp outcome({:failed, _}), do: "failed"
  defp outcome({:skipped, _}), do: "skipped"
  defp outcome({:excluded, _}), do: "skipped"
  defp outcome({:invalid, _}), do: "errored"

  defp skip_reason({:skipped, _}), do: %{skip_reason: "skipped"}
  defp skip_reason({:excluded, _}), do: %{skip_reason: "excluded"}
  defp skip_reason(_state), do: %{}

  defp increment(counts, key), do: Map.update(counts, key, 1, &(&1 + 1))

  defp path(describe) when is_binary(describe) and describe != "", do: [text(describe)]
  defp path(_describe), do: []

  defp relative_file(file) when is_binary(file) do
    relative = Path.relative_to_cwd(file)

    if relative != "" and byte_size(relative) <= @max_text_bytes and
         Path.type(relative) == :relative and not String.starts_with?(relative, "~") and
         ".." not in Path.split(relative),
       do: relative
  end

  defp relative_file(_file), do: nil

  # U+0000 is not allowed anywhere in a report, so it becomes U+FFFD (as in the
  # Jest reporter) before the text is cut to its byte limit.
  defp text(value) do
    case :binary.replace(value, <<0>>, "\uFFFD", [:global]) do
      "" -> "-"
      value when byte_size(value) <= @max_text_bytes -> value
      value -> value |> binary_part(0, @max_text_bytes) |> valid_prefix()
    end
  end

  defp valid_prefix(value) do
    if String.valid?(value),
      do: value,
      else: valid_prefix(binary_part(value, 0, byte_size(value) - 1))
  end

  defp integer(value) when is_integer(value) and abs(value) <= @max_safe_integer, do: value
  defp integer(_value), do: nil

  defp line(value) when is_integer(value) and value >= 1 and value <= @max_safe_integer,
    do: value

  defp line(_value), do: nil

  defp boolean(value) when is_boolean(value), do: value
  defp boolean(_value), do: nil

  defp filters(filters) do
    filters
    |> List.wrap()
    |> Enum.map(&normalize_filter/1)
    |> Enum.reject(&is_nil/1)
    |> Enum.take(@max_filters)
  end

  defp normalize_filter(tag) when is_atom(tag), do: normalize_filter({tag, true})

  defp normalize_filter({tag, value}) when is_atom(tag) and is_boolean(value) do
    if name = identifier(Atom.to_string(tag)), do: %{tag: name, value: value}
  end

  defp normalize_filter(_filter), do: nil

  # The grammar is matched here rather than through a regex stored in a module
  # attribute, which newer OTP releases refuse to compile.
  defp identifier(value) when is_binary(value) do
    if Regex.match?(~r/\A[A-Za-z0-9][A-Za-z0-9._-]{0,127}\z/, value), do: value
  end

  defp identifier(_value), do: nil

  # goodflows sets both variables together; unless both are valid, neither is
  # used and the runner's own partitioning applies.
  defp partition do
    with partition when is_binary(partition) <- present_env("GOODFLOWS_PARTITION"),
         true <- valid_partition?(partition),
         count when is_integer(count) <-
           positive_integer(present_env("GOODFLOWS_PARTITION_COUNT")) do
      {partition, count}
    else
      _missing_or_invalid -> mix_partition()
    end
  end

  # As with goodflows, the partition and its count are used together or not at
  # all; an unpartitioned run is the single partition `null` of `1`.
  defp mix_partition do
    with partition when is_binary(partition) <- present_env("MIX_TEST_PARTITION"),
         true <- valid_partition?(partition),
         count when is_integer(count) <- positive_integer(present_env("TEST_TIMING_PARTITIONS")) do
      {partition, count}
    else
      _missing_or_invalid -> {nil, 1}
    end
  end

  defp valid_partition?(partition), do: Regex.match?(~r/\A[A-Za-z0-9._-]{1,64}\z/, partition)

  defp positive_integer(nil), do: nil

  defp positive_integer(value) do
    with true <- Regex.match?(~r/\A[0-9]+\z/, value),
         integer when integer in 1..@max_safe_integer//1 <- String.to_integer(value) do
      integer
    else
      _invalid -> nil
    end
  end

  defp present_env(name) do
    case System.get_env(name) do
      "" -> nil
      value -> value
    end
  end
end
