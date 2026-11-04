defmodule Goodflows.ExUnitReporterTest do
  use ExUnit.Case, async: true

  import ExUnit.CaptureIO

  alias Goodflows.ExUnitReporter

  defmodule ThrowingCase do
    def __ex_unit__(:config), do: throw(:synthetic_throw)
  end

  defmodule ExitingCase do
    def __ex_unit__(:config), do: exit(:synthetic_exit)
  end

  @moduletag :tmp_dir

  @common ~w(event suite_id report_id received_offset_us)
  @keys %{
    "suite_started" => ~w(format started_at runner partition partition_count seed filters),
    "group_started" => ~w(group file execution_id started_offset_us),
    "group_finished" => ~w(group file execution_id started_offset_us duration_us outcome counts),
    "test_started" => ~w(group path name file line execution_id started_offset_us),
    "test_finished" =>
      ~w(group path name file line execution_id started_offset_us duration_us outcome),
    "suite_finished" => ~w(aborted unfinished_tests unfinished_groups counts duration_us),
    "interrupted" => [],
    "max_failures_reached" => []
  }
  @pairs %{"test_finished" => "test_started", "group_finished" => "group_started"}

  setup %{tmp_dir: directory} do
    formatter = start_supervised!({ExUnitReporter, test_timing_directory: directory})
    emit(formatter, {:suite_started, seed: 42, max_cases: 8})
    %{formatter: formatter, directory: directory}
  end

  test "streams records before suite completion and keeps executions distinct", ctx do
    first = test_case(%{parameters: %{mode: :first}})
    second = test_case(%{parameters: %{mode: :second}})

    for test <- [first, second, first] do
      emit(ctx.formatter, {:test_started, test})
      emit(ctx.formatter, {:test_finished, %{test | time: 125}})
    end

    records = records(ctx.directory)
    assert hd(records)["event"] == "suite_started"
    refute Enum.any?(records, &(&1["event"] == "suite_finished"))
    finished = Enum.filter(records, &(&1["event"] == "test_finished"))
    assert length(finished) == 3
    assert length(Enum.uniq_by(finished, & &1["execution_id"])) == 3
    assert [a, b, c] = Enum.map(finished, & &1["meta"]["parameter_id"])
    assert a == c
    refute a == b
    assert a == :erlang.phash2(%{mode: :first}, 4_294_967_296)
    assert Enum.all?([a, b], &(is_integer(&1) and &1 >= 0))

    for record <- finished do
      assert record["duration_us"] == 125
      assert record["outcome"] == "passed"
      assert record["group"] == "PrimaryCareTest"
      assert record["path"] == ["verify insurance"]
      assert record["name"] == "verifies insurance"
      assert record["file"] == "test/primary_care_test.exs"
      assert record["line"] == 12
      assert record["meta"]["async"] == true
      assert record["received_offset_us"] >= record["started_offset_us"]
    end

    assert_contract(records)
  end

  test "maps modules to groups with ExUnit details in meta", ctx do
    test = test_case()
    test_module = test_module(test)
    emit(ctx.formatter, {:module_started, test_module})
    emit(ctx.formatter, {:test_started, test})
    emit(ctx.formatter, {:test_finished, %{test | time: 1_200}})
    emit(ctx.formatter, {:module_finished, %{test_module | tests: [%{test | time: 1_200}]}})
    emit(ctx.formatter, {:suite_finished, %{run: 8_000, async: 3_000, load: 500}})

    records = records(ctx.directory)
    assert_contract(records)
    [started, finished] = Enum.filter(records, &(&1["event"] =~ ~r/^group_/))
    assert started["group"] == "PrimaryCareTest"
    assert started["meta"] == %{"async" => nil, "setup_all" => true, "parameter_id" => nil}
    assert finished["outcome"] == "passed"
    assert finished["counts"] == %{"passed" => 1}
    assert finished["duration_us"] >= 0

    summary = List.last(records)
    assert summary["counts"] == %{"passed" => 1}
    assert summary["duration_us"] == 8_000

    assert summary["meta"] == %{
             "run_us" => 8_000,
             "async_us" => 3_000,
             "sync_us" => 5_000,
             "load_us" => 500,
             "total_us" => 8_500
           }
  end

  test "writes a start when ExUnit reports a test without one", ctx do
    test = test_case(%{state: {:invalid, test_module(test_case())}})
    emit(ctx.formatter, {:test_finished, test})
    emit(ctx.formatter, {:module_finished, test_module(test)})
    emit(ctx.formatter, {:suite_finished, %{run: 100, async: nil, load: nil}})

    records = records(ctx.directory)
    assert_contract(records)

    assert Enum.map(records, & &1["event"]) ==
             ~w(suite_started test_started test_finished group_started group_finished suite_finished)

    assert List.last(records)["counts"] == %{"errored" => 1}
  end

  test "writes a group start when a module finishes without one", ctx do
    test = test_case()
    test_module = test_module(%{test | time: 10})
    emit(ctx.formatter, {:test_started, test})
    emit(ctx.formatter, {:test_finished, %{test | time: 10}})
    emit(ctx.formatter, {:module_finished, test_module})
    emit(ctx.formatter, {:suite_finished, %{run: 100, async: nil, load: nil}})

    records = records(ctx.directory)
    assert_contract(records)
    [started, finished] = Enum.filter(records, &(&1["event"] =~ ~r/^group_/))
    assert started["group"] == "PrimaryCareTest"
    assert started["execution_id"] == finished["execution_id"]
    assert started["meta"] == finished["meta"]
    assert finished["outcome"] == "passed"
    assert finished["counts"] == %{"passed" => 1}
    assert List.last(records)["unfinished_groups"] == 0
  end

  test "replaces U+0000 in text with U+FFFD", ctx do
    test =
      test_case(%{name: :"test verify\0insurance a\0b", tags: %{describe: "verify\0insurance"}})

    emit(ctx.formatter, {:test_started, test})
    emit(ctx.formatter, {:test_finished, %{test | time: 5}})

    report = File.read!(report(ctx.directory))
    refute report =~ "\\u0000"
    refute report =~ <<0>>
    records = records(ctx.directory)
    assert_contract(records)

    for record <- Enum.filter(records, &(&1["event"] in ["test_started", "test_finished"])) do
      assert record["name"] == "a\uFFFDb"
      assert record["path"] == ["verify\uFFFDinsurance"]
    end
  end

  test "bounds text fields to the contract on start and finish", ctx do
    long_describe = "a" <> String.duplicate("é", 3_000)
    # Test names are atoms (at most 255 characters), so only describe and
    # file can exceed the text limit.

    test =
      test_case(%{
        tags: %{describe: long_describe, file: "/elsewhere/primary_care_test.exs"}
      })

    emit(ctx.formatter, {:test_started, test})
    emit(ctx.formatter, {:test_finished, %{test | time: 5}})

    records = records(ctx.directory)
    assert_contract(records)

    for record <- Enum.filter(records, &(&1["event"] in ["test_started", "test_finished"])) do
      assert [describe] = record["path"]
      assert byte_size(describe) == 4_095
      assert String.valid?(describe)
      assert String.starts_with?(long_describe, describe)
      assert record["file"] == nil
    end
  end

  test "records null files outside the project or beyond the text limit", ctx do
    for file <- [
          "../outside_test.exs",
          "~/primary_care_test.exs",
          "test/" <> String.duplicate("a", 4_092) <> "_test.exs"
        ] do
      test = test_case(%{tags: %{file: file}})
      emit(ctx.formatter, {:module_started, test_module(test)})
      emit(ctx.formatter, {:test_started, test})
      emit(ctx.formatter, {:test_finished, %{test | time: 5}})
      emit(ctx.formatter, {:module_finished, test_module(test)})
    end

    records = records(ctx.directory)
    assert_contract(records)
    executions = Enum.reject(records, &(&1["event"] == "suite_started"))
    assert length(executions) == 12
    assert Enum.all?(executions, &(&1["file"] == nil))
  end

  test "maps nonexecuted outcomes onto the core model without durations", ctx do
    for {state, outcome, reason} <- [
          {{:skipped, "private reason"}, "skipped", "skipped"},
          {{:excluded, "private tag"}, "skipped", "excluded"},
          {{:invalid, test_module(test_case())}, "errored", nil}
        ] do
      test = test_case(%{state: state, time: 99_999})
      emit(ctx.formatter, {:test_started, test})
      emit(ctx.formatter, {:test_finished, test})
      finished = List.last(records(ctx.directory))
      assert finished["outcome"] == outcome
      assert finished["duration_us"] == nil
      assert finished["meta"]["skip_reason"] == reason
    end

    emit(ctx.formatter, {:suite_finished, %{run: 100, async: nil, load: nil}})
    records = records(ctx.directory)
    assert_contract(records)
    assert List.last(records)["counts"] == %{"skipped" => 2, "errored" => 1}
    refute File.read!(report(ctx.directory)) =~ "private"
  end

  test "a failing setup_all errors the group", ctx do
    test = test_case(%{state: {:invalid, test_module(test_case())}})

    test_module = %{
      test_module(test)
      | state: {:failed, [{:error, RuntimeError.exception("x"), []}]}
    }

    emit(ctx.formatter, {:module_started, test_module})
    emit(ctx.formatter, {:test_started, test})
    emit(ctx.formatter, {:test_finished, test})
    emit(ctx.formatter, {:module_finished, test_module})

    records = records(ctx.directory)
    assert_contract(records)
    assert Enum.find(records, &(&1["event"] == "test_finished"))["outcome"] == "errored"
    group = Enum.find(records, &(&1["event"] == "group_finished"))
    assert group["outcome"] == "errored"
    assert group["counts"] == %{"errored" => 1}
  end

  test "omits logs, parameter values, arbitrary tags and failure payloads", ctx do
    private = "Alex Sample private payload"
    test = test_case(%{parameters: %{patient: private}, logs: private, tags: %{patient: private}})
    test = %{test | state: {:failed, [{:error, RuntimeError.exception(private), []}]}, time: 12}
    emit(ctx.formatter, {:test_started, test})
    emit(ctx.formatter, {:test_finished, test})
    emit(ctx.formatter, {:suite_finished, %{run: 100, async: nil, load: nil}})

    refute File.read!(report(ctx.directory)) =~ private
    assert Enum.at(records(ctx.directory), 2)["outcome"] == "failed"
    assert List.last(records(ctx.directory))["counts"] == %{"failed" => 1}
  end

  test "allocates independent report files in the same directory", ctx do
    second =
      start_supervised!({ExUnitReporter, test_timing_directory: ctx.directory}, id: :second)

    emit(second, {:suite_started, seed: 43, max_cases: 1})
    paths = reports(ctx.directory)
    assert length(paths) == 2
    ids = Enum.map(paths, &first_record(&1)["report_id"])
    assert length(Enum.uniq(ids)) == 2
  end

  test "flags aborted runs and counts every unfinished execution", ctx do
    test = test_case()

    for _ <- 1..2 do
      emit(ctx.formatter, {:module_started, test_module(test)})
      emit(ctx.formatter, {:test_started, test})
    end

    emit(ctx.formatter, :max_failures_reached)
    emit(ctx.formatter, {:sigquit, []})
    emit(ctx.formatter, {:suite_finished, %{run: 100, async: nil, load: nil}})
    records = records(ctx.directory)
    assert_contract(records)
    assert Enum.count(records, &(&1["event"] == "max_failures_reached")) == 1
    assert Enum.count(records, &(&1["event"] == "interrupted")) == 1
    summary = List.last(records)
    assert summary["aborted"] == true
    assert summary["unfinished_tests"] == 2
    assert summary["unfinished_groups"] == 2
  end

  test "shares a suite identity across workers without sharing report identities", ctx do
    directory = Path.join(ctx.directory, "workers")

    for worker <- 1..2 do
      formatter =
        start_supervised!(
          {ExUnitReporter, test_timing_directory: directory, test_timing_suite_id: "nightly-42"},
          id: {:worker, worker}
        )

      emit(formatter, {:suite_started, seed: 42, max_cases: 1})
    end

    started = directory |> reports() |> Enum.map(&first_record/1)
    assert Enum.map(started, & &1["suite_id"]) == ["nightly-42", "nightly-42"]
    assert length(Enum.uniq_by(started, & &1["report_id"])) == 2
    assert Enum.all?(started, &identifier?(&1["report_id"]))
  end

  test "records boolean identifier filters in order and omits every other filter", ctx do
    private = "Alex Sample private filter"
    directory = Path.join(ctx.directory, "filters")
    long_tag = String.duplicate("a", 128)

    formatter =
      start_supervised!({ExUnitReporter, test_timing_directory: directory}, id: :filters)

    emit(
      formatter,
      {:suite_started,
       seed: 42,
       max_cases: 1,
       include: [{:line, 42}, {:patient, private}, :"verify-insurance.v2", {:review, true}],
       exclude: [
         :chrome,
         {:qpdf, false},
         :ok?,
         :chrome,
         :_private,
         String.to_atom(long_tag),
         String.to_atom(long_tag <> "a"),
         {"chrome", true}
       ]}
    )

    [started] = records(directory)

    assert started["filters"]["include"] == [
             %{"tag" => "verify-insurance.v2", "value" => true},
             %{"tag" => "review", "value" => true}
           ]

    assert started["filters"]["exclude"] == [
             %{"tag" => "chrome", "value" => true},
             %{"tag" => "qpdf", "value" => false},
             %{"tag" => "chrome", "value" => true},
             %{"tag" => long_tag, "value" => true}
           ]

    refute File.read!(report(directory)) =~ private
  end

  test "caps each filter list at 256 entries", ctx do
    directory = Path.join(ctx.directory, "capped")

    formatter =
      start_supervised!({ExUnitReporter, test_timing_directory: directory}, id: :capped)

    tags = [{:line, 1}, :ok? | for(index <- 1..257, do: String.to_atom("review-#{index}"))]
    emit(formatter, {:suite_started, seed: 42, max_cases: 1, include: tags, exclude: tags})
    [started] = records(directory)

    for list <- ["include", "exclude"] do
      assert length(started["filters"][list]) == 256
      assert List.last(started["filters"][list]) == %{"tag" => "review-256", "value" => true}
    end
  end

  test "resolves the report directory from the option, then goodflows, then the default", ctx do
    explicit = Path.join(ctx.directory, "explicit")
    goodflows = Path.join(ctx.directory, "goodflows")
    project = Path.join(ctx.directory, "project")
    File.mkdir_p!(project)

    start_formatters(
      [
        {[{"GOODFLOWS_REPORT_DIR", goodflows}], [test_timing_directory: explicit]},
        {[{"GOODFLOWS_REPORT_DIR", goodflows}], []},
        {[{"GOODFLOWS_REPORT_DIR", ""}], []},
        {[], []}
      ],
      project
    )

    assert length(reports(explicit)) == 1
    assert length(reports(goodflows)) == 1
    default = reports(Path.join(project, "_build/test/timings"))
    assert length(default) == 2

    for path <- reports(explicit) ++ reports(goodflows) ++ default do
      assert %{"event" => "suite_started", "format" => "test-timings/1"} = first_record(path)
    end
  end

  test "takes partitions from goodflows, then mix test partitions, then none, each as a pair",
       ctx do
    cases = [
      {"direct", [], nil, 1},
      {"goodflows", [{"GOODFLOWS_PARTITION", "2"}, {"GOODFLOWS_PARTITION_COUNT", "3"}], "2", 3},
      {"goodflows-first",
       [
         {"GOODFLOWS_PARTITION", "2"},
         {"GOODFLOWS_PARTITION_COUNT", "3"},
         {"MIX_TEST_PARTITION", "4"},
         {"TEST_TIMING_PARTITIONS", "5"}
       ], "2", 3},
      {"bad-goodflows",
       [
         {"GOODFLOWS_PARTITION", "a b"},
         {"GOODFLOWS_PARTITION_COUNT", "3"},
         {"MIX_TEST_PARTITION", "4"},
         {"TEST_TIMING_PARTITIONS", "5"}
       ], "4", 5},
      {"goodflows-no-count",
       [
         {"GOODFLOWS_PARTITION", "2"},
         {"MIX_TEST_PARTITION", "4"},
         {"TEST_TIMING_PARTITIONS", "5"}
       ], "4", 5},
      {"goodflows-bad-count",
       [
         {"GOODFLOWS_PARTITION", "2"},
         {"GOODFLOWS_PARTITION_COUNT", "0"},
         {"MIX_TEST_PARTITION", "4"},
         {"TEST_TIMING_PARTITIONS", "5"}
       ], "4", 5},
      {"goodflows-count-only", [{"GOODFLOWS_PARTITION_COUNT", "3"}], nil, 1},
      {"goodflows-no-count-unpartitioned", [{"GOODFLOWS_PARTITION", "2"}], nil, 1},
      {"empty", [{"MIX_TEST_PARTITION", ""}, {"TEST_TIMING_PARTITIONS", "3"}], nil, 1},
      {"partitioned", [{"MIX_TEST_PARTITION", "2"}, {"TEST_TIMING_PARTITIONS", "3"}], "2", 3},
      {"namespace", [{"MIX_TEST_PARTITION", "ci_7.a-b"}, {"TEST_TIMING_PARTITIONS", "8"}],
       "ci_7.a-b", 8},
      {"no-count", [{"MIX_TEST_PARTITION", "ci_7.a-b"}], nil, 1},
      {"empty-count", [{"MIX_TEST_PARTITION", "1"}, {"TEST_TIMING_PARTITIONS", ""}], nil, 1},
      {"count-only", [{"TEST_TIMING_PARTITIONS", "3"}], nil, 1},
      {"bad-partition", [{"MIX_TEST_PARTITION", "ci/7"}, {"TEST_TIMING_PARTITIONS", "3"}], nil,
       1},
      {"long-partition",
       [{"MIX_TEST_PARTITION", String.duplicate("p", 65)}, {"TEST_TIMING_PARTITIONS", "3"}], nil,
       1},
      {"zero-count", [{"MIX_TEST_PARTITION", "1"}, {"TEST_TIMING_PARTITIONS", "0"}], nil, 1},
      {"max-count", [{"MIX_TEST_PARTITION", "1"}, {"TEST_TIMING_PARTITIONS", "9007199254740991"}],
       "1", 9_007_199_254_740_991},
      {"unsafe-count",
       [{"MIX_TEST_PARTITION", "1"}, {"TEST_TIMING_PARTITIONS", "9007199254740992"}], nil, 1},
      {"signed-count", [{"MIX_TEST_PARTITION", "1"}, {"TEST_TIMING_PARTITIONS", "+3"}], nil, 1}
    ]

    start_formatters(
      for {name, env, _partition, _count} <- cases do
        {env, [test_timing_directory: Path.join(ctx.directory, name)]}
      end,
      ctx.directory
    )

    for {name, _env, partition, count} <- cases do
      [path] = reports(Path.join(ctx.directory, name))
      started = first_record(path)
      assert {name, started["partition"], started["partition_count"]} == {name, partition, count}
    end
  end

  test "takes the suite identity only from a valid suite id", ctx do
    cases = [
      {"run-id", [{"GOODFLOWS_RUN_ID", "0b7d3f4e-8c1a-4f5e-9d2b-6a7c8e9f0a1b"}], [],
       "0b7d3f4e-8c1a-4f5e-9d2b-6a7c8e9f0a1b"},
      {"env-over-run-id", [{"TEST_TIMING_SUITE_ID", "suite-a"}, {"GOODFLOWS_RUN_ID", "run-1"}],
       [], "suite-a"},
      {"invalid-env-then-run-id",
       [{"TEST_TIMING_SUITE_ID", "suite/a"}, {"GOODFLOWS_RUN_ID", "run-1"}], [], "run-1"},
      {"invalid-run-id", [{"GOODFLOWS_RUN_ID", "run/1"}], [], nil},
      {"empty-run-id", [{"GOODFLOWS_RUN_ID", ""}], [], nil},
      {"env", [{"TEST_TIMING_SUITE_ID", "suite-a"}], [], "suite-a"},
      {"option", [{"TEST_TIMING_SUITE_ID", "suite-a"}, {"GOODFLOWS_RUN_ID", "run-1"}],
       [test_timing_suite_id: "suite-b"], "suite-b"},
      {"invalid-option", [{"TEST_TIMING_SUITE_ID", "suite-a"}], [test_timing_suite_id: "a/b"],
       "suite-a"},
      {"retired", [{"TEST_TIMING_RUN_ID", "suite-a"}], [test_timing_run_id: "suite-b"], nil},
      {"empty", [{"TEST_TIMING_SUITE_ID", ""}], [], nil},
      {"invalid", [{"TEST_TIMING_SUITE_ID", "suite/a"}], [], nil},
      {"leading-dot", [{"TEST_TIMING_SUITE_ID", ".suite-a"}], [], nil},
      {"too-long", [{"TEST_TIMING_SUITE_ID", String.duplicate("s", 129)}], [], nil}
    ]

    start_formatters(
      for {name, env, opts, _suite_id} <- cases do
        {env, [test_timing_directory: Path.join(ctx.directory, name)] ++ opts}
      end,
      ctx.directory
    )

    for {name, _env, _opts, suite_id} <- cases do
      [path] = reports(Path.join(ctx.directory, name))
      started = first_record(path)
      assert {name, started["suite_id"]} == {name, suite_id || started["report_id"]}
    end
  end

  test "names a test by its own title, without ExUnit's test and describe prefix", ctx do
    for {name, describe, expected} <- [
          {:"test math adds", "math", "adds"},
          {:"test adds", nil, "adds"},
          {:"test math", nil, "math"},
          {:"test mathematics adds", "math", "mathematics adds"},
          {:"test math ", "math", "-"},
          {:"test ", nil, "-"},
          {:"doctest Fixture.add/2 (1)", nil, "doctest Fixture.add/2 (1)"},
          {:"property adds", nil, "property adds"}
        ] do
      test = test_case(%{name: name, tags: %{describe: describe}})
      emit(ctx.formatter, {:test_started, test})
      emit(ctx.formatter, {:test_finished, %{test | time: 5}})
      [started, finished] = Enum.take(records(ctx.directory), -2)
      assert {name, started["name"], finished["name"]} == {name, expected, expected}
    end

    assert_contract(records(ctx.directory))
  end

  test "takes async from the module for tests the runner did not tag", ctx do
    test = test_case(%{module: __MODULE__, state: {:excluded, "private tag"}})
    test = %{test | tags: Map.delete(test.tags, :async)}
    emit(ctx.formatter, {:test_started, test})
    emit(ctx.formatter, {:test_finished, test})
    assert List.last(records(ctx.directory))["meta"]["async"] == true
  end

  test "counts excluded and skipped tests in their group", ctx do
    passed = test_case(%{name: :"test verify insurance passes", time: 5})
    skipped = test_case(%{name: :"test verify insurance skips", state: {:skipped, "x"}})
    other = test_case(%{module: OtherCareTest, state: {:excluded, "x"}})

    emit(ctx.formatter, {:module_started, test_module(passed)})
    emit(ctx.formatter, {:module_started, test_module(other)})

    for test <- [skipped, other, passed] do
      emit(ctx.formatter, {:test_started, test})
      emit(ctx.formatter, {:test_finished, test})
    end

    emit(ctx.formatter, {:module_finished, test_module(passed)})
    emit(ctx.formatter, {:module_finished, %{test_module(other) | tests: []}})
    emit(ctx.formatter, {:module_started, test_module(passed)})
    emit(ctx.formatter, {:module_finished, %{test_module(passed) | tests: []}})

    records = records(ctx.directory)
    assert_contract(records)

    assert records
           |> Enum.filter(&(&1["event"] == "group_finished"))
           |> Enum.map(&{&1["group"], &1["outcome"], &1["counts"]}) == [
             {"PrimaryCareTest", "passed", %{"passed" => 1, "skipped" => 1}},
             {"OtherCareTest", "skipped", %{"skipped" => 1}},
             {"PrimaryCareTest", "passed", %{}}
           ]
  end

  test "counts unparameterized skips in every interleaved parameter group", ctx do
    skipped = test_case(%{name: :"test verify insurance skips", state: {:skipped, "x"}})
    groups = for mode <- [:a, :b], do: test_case(%{parameters: %{mode: mode}, time: 5})

    for test <- groups, do: emit(ctx.formatter, {:module_started, test_module(test)})

    for test <- groups ++ [skipped, skipped] do
      emit(ctx.formatter, {:test_started, test})
      emit(ctx.formatter, {:test_finished, test})
    end

    for test <- groups, do: emit(ctx.formatter, {:module_finished, test_module(test)})
    emit(ctx.formatter, {:suite_finished, %{run: 100, async: nil, load: nil}})

    records = records(ctx.directory)
    assert_contract(records)

    assert records
           |> Enum.filter(&(&1["event"] == "group_finished"))
           |> Enum.map(& &1["counts"]) == [
             %{"passed" => 1, "skipped" => 1},
             %{"passed" => 1, "skipped" => 1}
           ]

    assert List.last(records)["counts"] == %{"passed" => 2, "skipped" => 2}
    assert :sys.get_state(ctx.formatter).skips == %{}
  end

  test "describes the reporter and whether the run filtered tests", ctx do
    [started] = records(ctx.directory)
    assert started["meta"]["reporter"] == "goodflows_exunit_reporter"
    assert started["meta"]["reporter_version"] == Mix.Project.config()[:version]
    assert started["meta"]["filtered"] == false

    for {name, opts, filtered} <- [
          {"include", [include: [:review], exclude: []], true},
          {"exclude", [include: [], exclude: [:chrome]], true},
          {"ids", [only_test_ids: MapSet.new([{PrimaryCareTest, :"test a"}])], true},
          {"id-list", [only_test_ids: [{PrimaryCareTest, :"test a"}]], true},
          {"no-ids", [include: [], exclude: [], only_test_ids: MapSet.new()], false},
          {"nil-ids", [only_test_ids: nil], false}
        ] do
      directory = Path.join(ctx.directory, name)

      formatter =
        start_supervised!({ExUnitReporter, test_timing_directory: directory}, id: name)

      emit(formatter, {:suite_started, [seed: 1, max_cases: 1] ++ opts})
      [started] = records(directory)
      assert {name, started["meta"]["filtered"]} == {name, filtered}
      assert map_size(started["meta"]) <= 32
    end
  end

  test "a throw or exit in the reporter warns once and stops writing", ctx do
    for {name, module} <- [{"throw", ThrowingCase}, {"exit", ExitingCase}] do
      directory = Path.join(ctx.directory, name)

      formatter =
        start_supervised!({ExUnitReporter, test_timing_directory: directory}, id: name)

      emit(formatter, {:suite_started, seed: 1, max_cases: 1})
      test_module = %{test_module(test_case(%{module: module})) | name: module}

      warning =
        capture_io(:stderr, fn ->
          emit(formatter, {:module_started, test_module})
          emit(formatter, {:module_finished, test_module})
        end)

      assert warning =~ "Goodflows.ExUnitReporter stopped writing"
      assert warning =~ "synthetic_#{name}"
      assert length(Regex.scan(~r/stopped writing/, warning)) == 1
      assert Enum.map(records(directory), & &1["event"]) == ["suite_started"]
    end
  end

  test "configure/0 adds the reporter when no formatters are configured", ctx do
    output =
      run_script(
        """
        Application.delete_env(:ex_unit, :formatters)
        Goodflows.ExUnitReporter.configure()
        IO.inspect(ExUnit.configuration()[:formatters], label: "formatters")
        """,
        ctx.directory
      )

    assert output =~ "formatters: [Goodflows.ExUnitReporter]"
  end

  test "warns when the option keeps the report from goodflows, and still writes it", ctx do
    goodflows = Path.join(ctx.directory, "goodflows")
    explicit = Path.join(ctx.directory, "explicit")
    same = Path.join(ctx.directory, "same")

    output =
      start_formatters(
        [
          {[{"GOODFLOWS_REPORT_DIR", goodflows}], [test_timing_directory: explicit]},
          {[{"GOODFLOWS_REPORT_DIR", same}], [test_timing_directory: same <> "/."]},
          {[{"GOODFLOWS_REPORT_DIR", ""}], [test_timing_directory: same]},
          {[], [test_timing_directory: same]}
        ],
        ctx.directory
      )

    assert output ==
             "warning: Goodflows.ExUnitReporter writes to #{explicit} (:test_timing_directory), " <>
               "not GOODFLOWS_REPORT_DIR (#{goodflows}), so goodflows will not collect this report\n"

    assert length(reports(explicit)) == 1
    assert length(reports(same)) == 3
    assert reports(goodflows) == []
  end

  test "maps a real ExUnit run onto the core model and keeps CLI output", ctx do
    directory = Path.join(ctx.directory, "real-exunit")

    {output, status} =
      run_suite(directory, [{"MIX_TEST_PARTITION", "7"}, {"TEST_TIMING_PARTITIONS", "8"}])

    assert status == 1, output
    assert output =~ "Finished in"
    records = records(directory)
    assert_contract(records)
    started = hd(records)

    assert started["runner"] == %{"name" => "exunit", "version" => System.version()}
    assert {:ok, _started_at, 0} = DateTime.from_iso8601(started["started_at"])
    assert started["partition"] == "7"
    assert started["partition_count"] == 8
    assert started["seed"] == 42
    assert started["meta"]["max_cases"] == 2
    assert started["meta"]["schedulers"] > 0
    assert started["meta"]["elixir_version"] == System.version()
    assert started["meta"]["otp_release"] == System.otp_release()
    assert started["meta"]["reporter"] == "goodflows_exunit_reporter"
    assert started["meta"]["reporter_version"] == Mix.Project.config()[:version]
    assert started["meta"]["filtered"] == true
    assert started["filters"]["include"] == []

    assert started["filters"]["exclude"] == [
             %{"tag" => "chrome", "value" => true},
             %{"tag" => "slow", "value" => true}
           ]

    finished = Enum.filter(records, &(&1["event"] == "test_finished"))
    excluded = Enum.filter(finished, &(&1["name"] == "excluded browser review"))
    assert length(excluded) == 2
    assert Enum.all?(excluded, &(&1["outcome"] == "skipped"))
    assert Enum.all?(excluded, &(&1["meta"]["skip_reason"] == "excluded"))
    assert Enum.all?(excluded, &(&1["meta"]["async"] == true))

    all_skipped = Enum.filter(finished, &(&1["group"] == "AllSkippedTimingExample"))
    assert Enum.map(all_skipped, & &1["meta"]["async"]) == [false, false]

    invalid = Enum.find(finished, &(&1["group"] == "InvalidTimingExample"))
    assert invalid["outcome"] == "errored"
    group = &Enum.find(records, fn r -> r["event"] == "group_finished" and r["group"] == &1 end)
    outcome_counts = &Map.take(group.(&1), ["outcome", "counts"])

    assert outcome_counts.("InvalidTimingExample") == %{
             "outcome" => "errored",
             "counts" => %{"errored" => 1}
           }

    assert outcome_counts.("FailedTimingExample") == %{
             "outcome" => "failed",
             "counts" => %{"failed" => 1}
           }

    assert outcome_counts.("SyncTimingExample") == %{
             "outcome" => "passed",
             "counts" => %{"passed" => 1, "skipped" => 1}
           }

    assert outcome_counts.("AllSkippedTimingExample") == %{
             "outcome" => "skipped",
             "counts" => %{"skipped" => 2}
           }

    primary =
      Enum.filter(
        records,
        &(&1["event"] == "group_finished" and &1["group"] == "PrimaryCareTimingExample")
      )

    assert length(primary) == 2

    assert Enum.all?(
             primary,
             &(&1["outcome"] == "passed" and &1["counts"] == %{"passed" => 1, "skipped" => 2})
           )

    nested = Enum.find(finished, &(&1["name"] == "verifies"))
    assert nested["path"] == ["insurance"]

    passed =
      Enum.filter(
        finished,
        &(&1["group"] == "PrimaryCareTimingExample" and &1["outcome"] == "passed")
      )

    assert length(Enum.uniq_by(passed, & &1["meta"]["parameter_id"])) == 2
    assert Enum.all?(passed, &(&1["meta"]["async"] == true and is_integer(&1["duration_us"])))

    summary = List.last(records)
    assert summary["event"] == "suite_finished"
    assert summary["counts"] == %{"passed" => 3, "skipped" => 7, "errored" => 1, "failed" => 1}

    group_totals =
      records
      |> Enum.filter(&(&1["event"] == "group_finished"))
      |> Enum.reduce(%{}, &Map.merge(&2, &1["counts"], fn _outcome, a, b -> a + b end))

    assert group_totals == summary["counts"]
    assert summary["unfinished_tests"] == 0
    assert summary["unfinished_groups"] == 0

    assert Enum.all?(
             ~w(run_us async_us sync_us load_us total_us),
             &Map.has_key?(summary["meta"], &1)
           )
  end

  test "an unwritable directory warns once and never changes results", ctx do
    {expected, expected_status} = run_suite(Path.join(ctx.directory, "baseline"), [])
    blocker = Path.join(ctx.directory, "blocker")
    File.write!(blocker, "")
    {output, status} = run_suite(Path.join(blocker, "reports"), [])

    assert status == expected_status
    assert Regex.scan(~r/Goodflows.ExUnitReporter stopped writing/, output) |> length() == 1
    summary = &Regex.run(~r/\d+ tests?, .*/, &1)
    assert summary.(output) == summary.(expected)
    refute File.exists?(Path.join(blocker, "reports"))
  end

  defp run_suite(directory, env) do
    script = """
    ExUnit.start(autorun: false, seed: 42, max_cases: 2,
      include: [ticket: "synthetic-1"], exclude: [:chrome, :slow, :"ok?"],
      formatters: [ExUnit.CLIFormatter],
      test_timing_directory: #{inspect(directory)})
    Goodflows.ExUnitReporter.configure()
    Goodflows.ExUnitReporter.configure()

    defmodule PrimaryCareTimingExample do
      use ExUnit.Case, async: true,
        parameterize: [%{mode: :insurance}, %{mode: :clinical}]
      describe "insurance" do
        test "verifies", do: assert(true)
      end
      @tag :skip
      test "skipped clinical review", do: assert(false)
      @tag :chrome
      test "excluded browser review", do: assert(false)
    end

    defmodule InvalidTimingExample do
      use ExUnit.Case
      setup_all do
        raise "synthetic setup failure"
      end
      test "invalid review", do: assert(true)
    end

    defmodule FailedTimingExample do
      use ExUnit.Case
      test "failed review", do: assert(false)
    end

    defmodule SyncTimingExample do
      use ExUnit.Case, async: false
      test "verifies referral", do: assert(true)
      @tag :slow
      test "excluded slow referral", do: assert(false)
    end

    defmodule AllSkippedTimingExample do
      use ExUnit.Case, async: false
      @tag :skip
      test "skipped intake", do: assert(false)
      @tag :skip
      test "skipped discharge", do: assert(false)
    end

    result = ExUnit.run()
    System.halt(if result.failures > 0, do: 1, else: 0)
    """

    System.cmd(
      System.find_executable("elixir"),
      ["--erl", "+S 2:2", "-pa", ebin(), "-e", script],
      env: isolated_env() ++ env,
      stderr_to_stdout: true
    )
  end

  defp start_formatters(cases, directory) do
    run_script(
      """
      for {env, opts} <- #{inspect(cases, limit: :infinity)} do
        for {name, value} <- env, do: System.put_env(name, value)
        {:ok, formatter} = Goodflows.ExUnitReporter.start_link(opts)
        GenServer.cast(formatter, {:suite_started, [seed: 42, max_cases: 1]})
        :sys.get_state(formatter)
        GenServer.stop(formatter)
        for {name, _value} <- env, do: System.delete_env(name)
      end
      """,
      directory
    )
  end

  defp run_script(script, directory) do
    {output, status} =
      System.cmd(System.find_executable("elixir"), ["-pa", ebin(), "-e", script],
        cd: directory,
        env: isolated_env(),
        stderr_to_stdout: true
      )

    assert status == 0, output
    output
  end

  # Every record carries exactly its event's keys (plus optional meta), and
  # every finished event repeats its open start's core values.
  defp assert_contract(records) do
    Enum.reduce(records, %{}, fn record, open ->
      keys = @common ++ Map.fetch!(@keys, record["event"])
      assert Enum.sort(Map.keys(Map.delete(record, "meta"))) == Enum.sort(keys), inspect(record)
      assert record["suite_id"] == hd(records)["suite_id"]
      assert record["report_id"] == hd(records)["report_id"]

      case record["event"] do
        event when event in ["test_started", "group_started"] ->
          key = {event, record["execution_id"]}
          refute Map.has_key?(open, key)
          Map.put(open, key, record)

        event when event in ["test_finished", "group_finished"] ->
          key = {@pairs[event], record["execution_id"]}
          assert {started, open} = Map.pop(open, key)
          fields = Map.fetch!(@keys, @pairs[event])
          assert Map.take(record, fields) == Map.take(started, fields)
          open

        _event ->
          open
      end
    end)
  end

  defp isolated_env do
    for name <-
          ~w(GOODFLOWS_REPORT_DIR GOODFLOWS_RUN_ID GOODFLOWS_PARTITION GOODFLOWS_PARTITION_COUNT
             MIX_TEST_PARTITION TEST_TIMING_PARTITIONS TEST_TIMING_SUITE_ID TEST_TIMING_RUN_ID),
        do: {name, nil}
  end

  defp ebin, do: Path.dirname(:code.which(ExUnitReporter))

  defp reports(directory), do: Path.wildcard(Path.join(directory, "*.jsonl"))

  defp report(directory) do
    [path] = reports(directory)
    path
  end

  defp first_record(path),
    do: path |> File.read!() |> String.split("\n", trim: true) |> hd() |> JSON.decode!()

  defp identifier?(value),
    do: is_binary(value) and Regex.match?(~r/\A[A-Za-z0-9][A-Za-z0-9._-]{0,127}\z/, value)

  defp emit(formatter, event) do
    GenServer.cast(formatter, event)
    :sys.get_state(formatter)
  end

  defp records(directory) do
    directory
    |> report()
    |> File.read!()
    |> String.split("\n", trim: true)
    |> Enum.map(&JSON.decode!/1)
  end

  defp test_case(overrides \\ %{}) do
    tags =
      Map.merge(
        %{
          file: Path.expand("test/primary_care_test.exs"),
          line: 12,
          describe: "verify insurance",
          async: true
        },
        Map.get(overrides, :tags, %{})
      )

    struct!(
      ExUnit.Test,
      Map.merge(
        %{module: PrimaryCareTest, name: :"test verify insurance verifies insurance", tags: tags},
        Map.put(overrides, :tags, tags)
      )
    )
  end

  defp test_module(test) do
    %ExUnit.TestModule{
      name: test.module,
      file: test.tags.file,
      setup_all?: true,
      parameters: test.parameters,
      tests: [test]
    }
  end
end
