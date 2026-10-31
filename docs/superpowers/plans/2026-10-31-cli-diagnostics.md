# CLI diagnostics and noninteractive initialization

Implement the seven requested changes and the accepted refinements using the
existing modules. Work continues on the clean `feat/cli-improvements` branch.

## Requirements and sequence

1. Init never waits on nonterminal input. Accept an immediately available complete
   y/yes line (case insensitive), or y/yes followed by EOF. No available data,
   partial line without EOF, and other responses cancel with exit 0 and an
   init_cancelled event. Keep interactive confirmation and --force. Bound input
   to 4 KiB and restore any descriptor flags changed for nonblocking reading.
2. Generic init succeeds but its generated job prints the replacement instruction
   on stderr and exits 1 until edited.
3. Every job result has command. Failed/timed_out results have separate stdout_tail
   and stderr_tail capped at 4096 UTF-8 bytes each, plus stdout_truncated and
   stderr_truncated. Capture raw suffixes incrementally in bounded buffers, decode
   lossily, retain valid UTF-8 boundaries, and cap the final rendered strings too.
   Preserve full events and raw logs. Human failure output shows the command/tails.
4. List JSON includes every Job field and settings {fail_fast, parallelism, workdir}
   with CLI overrides and logical-CPU resolution, exactly as run uses. Human list
   shows those fields too. Config serialization omits no fields from inspection.
5. Report one actual dependency cycle path, not merely unresolved vertices.
6. config_valid retains jobs (selected count), adds requested_jobs (explicit CLI
   names), selected_jobs (closure in config order), total_defined_jobs. Entire
   config validation remains before selection. Human output distinguishes counts.
7. Generated TOML omits empty depends_on/env, shell=true, unset timeout_secs only;
   retain nondefault values and workdir. Add a prominent comment explaining omitted
   parallelism uses every logical CPU and recommending setting it to machine size.

## Execution and verification

- Write and run failing CLI regressions for requirements 1/2/7, then implement.
- Write and run failing inspection/cycle regressions for 4/5/6, then implement.
- Write and run failing result/tail regressions for 3, then implement.
- Update README with exact new behavior and JSON fields. Run full tests, Rust 1.88
  fmt/Clippy/tests, release build, goodci self-check, native packaging smoke checks.
- Obtain an independent review as required by requesting-code-review; reproduce
  and fix substantive findings. Audit every numbered requirement before completion.

No push, tag, or release publication. Preserve bounded output/backpressure and
process-cleanup behavior established by the existing regression suite.

## Completion audit

All seven requirements are complete. Regression tests were run before each
implementation group and failed for the missing behavior, then passed afterward.

| Requirement | Authoritative evidence |
|---|---|
| Nonblocking init | `nonterminal_init_never_waits_for_unavailable_or_incomplete_input` exercises open streams with no input, incomplete answers, available complete answers, and oversized input; checks inherited status flags. File input tests cover y/yes at EOF. A pseudo-terminal test preserves interactive confirmation. |
| Generic placeholder fails | `generic_placeholder_fails_until_replaced` checks exit 1, stderr output event, and unsuccessful summary. Native packaged CLI smoke test verifies the same. |
| Commands and bounded tails | `failed_results_include_command_and_separate_bounded_output_tails` verifies exact 4096-byte suffixes, truncation, job_finished/summary equality, and unchanged full events/logs. `commands_are_in_all_results_and_timeout_tails_keep_short_output` covers passed, skipped, failed, and timed-out jobs. UTF-8/lossy expansion regression verifies rendered bounds and character alignment. Existing cancellation tests remain green. |
| Full list/effective settings | `list_shows_all_job_fields_and_effective_settings` checks both human and JSON output, including explicit defaults. `automatic_parallelism_is_resolved_for_list_as_for_run` verifies CPU resolution and the shared run/list resolver. |
| Actual cycle paths | `cycle_error_names_one_actual_cycle_without_downstream_jobs` reports a -> b -> a while excluding an innocent downstream job. Iterative DFS avoids recursion limits. |
| Validation selection metadata | `validation_describes_requested_selected_and_total_jobs` covers filtered and unfiltered validation and human counts/names; existing whole-config validation tests continue to reject invalid unselected jobs. |
| Clean generated TOML | `generated_toml_omits_defaults_and_explains_parallelism` checks omission of default job fields, retained nonempty dependencies/environment, comments, and parse/validation roundtrip. JSON list retains every field. |

Independent review found no substantive issues and ran 27 focused integration
tests plus additional pipe/EOF/descriptor-boundary checks. Completion audit found
one additional requirement edge case: confirmation must precede template
detection so declining an existing file works even with an ambiguous project or
invalid package manifest. A failing regression reproduced it; moving detection
after confirmation fixed it, with the whole suite green afterward.

Final verification:

- Default toolchain and Rust 1.88.0: **45 integration tests passed** on each.
- Rust 1.88 fmt and Clippy (`--locked --all-targets -- -D warnings`): passed.
- Rust 1.88 locked release build: passed.
- Goodci's own config: fmt, clippy, test, and doc all passed.
- Native x86_64 Linux release packaging, archive checksum, extracted CLI init,
  validation/list, failed-placeholder/successful replacement, and ambiguous-project
  cancellation checks: passed.
- `git diff --check`: passed. README documents all new behavior and JSON fields.

No remote publication performed. macOS runtime behavior is reviewed and covered
by the existing CI matrix; this session executed Linux checks locally.
