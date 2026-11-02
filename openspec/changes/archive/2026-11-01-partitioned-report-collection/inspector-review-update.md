# Inspector Review-Update — partitioned-report-collection

**Reviewed:** 2026-11-01
**Reviewer:** inspector-review-update
**Mode:** auto
**Verdict:** Ready to implement (after `add-job-partitions`)

## Summary

The change gives each instance of a partitioned reporting job its own report directory and makes Goodflows authoritative for partition metadata. As written, the envelope-only override would have been rejected by both the local snapshot re-validation (`identity_mismatch`) and the server (a 422, which is a permanent rejection). The design also assumed freezing happened after collection, but the code freezes snapshots as they are built. It left `pending/` and `acks/` unspecified per instance, which would have caused snapshot index collisions. All findings are patched.

**Original counts:** Critical: 2 · Warning: 6 · Suggestion: 2
**Patches:** Auto-patched: 6 · User-guided: 0 · Model-recommended: 4 · Skipped: 0

## Scope inspected

- Proposal: `openspec/changes/partitioned-report-collection/proposal.md`
- Design: `openspec/changes/partitioned-report-collection/design.md`
- Tasks: `openspec/changes/partitioned-report-collection/tasks.md`
- Deltas:
  - `openspec/changes/partitioned-report-collection/specs/test-timing-publishing/spec.md` (MODIFIED and ADDED)
  - `openspec/changes/partitioned-report-collection/specs/job-partitions/spec.md` (REMOVED)
- Canonical specs consulted: `openspec/specs/test-timing-publishing/spec.md`
- Other active changes consulted: `add-job-partitions` (post-review), `generalize-test-report-filters`
- Code verified: `src/reporting/{mod,collect,replay}.rs`, `src/output.rs`, `src/runner.rs`. Server: `../goodflows_web/app/lib/good_flows/test_reports/consistency.ex`, `../goodflows_web/openspec/changes/archive/2026-11-01-store-test-timings/design.md`. GoodCases: `../goodcases/openspec/changes/adopt-goodflows-partitions/`.

## Patches applied

6 findings were auto-patched and 4 were patched from model recommendations. None were skipped.

### Auto-patched

1. **(Critical) Instance `pending/` and `acks/` were unspecified.** `design.md` D1, proposal, spec, task 1.1. Each `SpoolSink` numbers snapshots from 0 (`src/reporting/mod.rs` `SpoolSink`), so a shared `pending/` would collide. The layout is now `<job>/p<i>/{reports,pending,acks}`, and `JobDirs` checks symlinks at both levels. Added an "Instance state directories" scenario.
2. **Stale Context section.** `design.md:5`. Rewritten from the actual code: the `create_run` layout, the `Run.env` → `runner_env` flow, `finish_with` matching by name, `collect_run`/`collect_dir`, streaming `SpoolSink::freeze` and `discard`, `validate_envelope`'s `identity_mismatch`, per-call caps, and replay's `dir == path_component(name)` check. `upload-test-timings` is described as archived (proposal Ordering and Migration Plan).
3. **Manifest serde details.** `design.md` D2, task 1.2. `ManifestJob` is `deny_unknown_fields`. Specified `#[serde(default, skip_serializing_if)]`, `dir` unchanged with the instance path derived from it, and `MANIFEST_VERSION` kept at 1.
4. **Replay was not covered.** `design.md` D2/D5, spec, task 3.2. `replay::upload_with` derives `JobDirs` from `dir` and checks report_id uniqueness per entry. Both now cover instances. Replay also emits `report_upload_pending` itself.
5. **Event sources.** `design.md` D5, task 3.1. `partition` is carried on `Item` so that `deliver` and replay can emit it. The text renderers live in `src/output.rs`.
6. **Impact list and README section names.** `proposal.md` Impact, task 1.3. Now lists `replay.rs` and the concrete functions, and names the README section "Test timing reports" → "Spool layout and retention".

### Model-recommended patches

1. **(Critical) Override must also rewrite `suite_started`.** `design.md` D3, proposal, spec "Goodflows supplies partition metadata", task 2.1.
   - **Chose:** set `partition`/`partition_count` in both the envelope and the first `suite_started` record, and change no other record field.
   - **Rationale:** `validate_envelope` and the server's `Consistency.partition/3` both require the envelope to equal the initial suite_started values. An envelope-only override is rejected locally, or with a permanent 422. The proposal's intent ("overriding producer values", Goodflows authoritative) rules out the other option of rejecting mismatches.
2. **Job-wide duplicate pass and streaming freeze.** `design.md` D4, spec, task 2.2.
   - **Chose:** keep streaming freeze, run the job-wide multiplicity pass in `collect_run`, and remove frozen duplicates via `Sink::discard` before the manifest is finalized. The spec now says "no snapshot remains" rather than "none written".
   - **Rationale:** the canonical spec requires at most one body in memory and freezing as each body is built. Discard is the mechanism the per-directory check already uses. Freeze-after-pass would need held bodies or a double parse.
3. **Per-job caps across instances.** New `design.md` D4a, spec scenario "Per-job caps span instances", proposal, task 2.2.
   - **Chose:** the 256-report, 256 MiB and entry caps span all instances of a job, consumed in index order.
   - **Rationale:** the canonical "Report processing is bounded" requirement says "per job". Per-directory caps would let a 64-partition job read up to 16 GiB. This keeps the canonical spec unmodified.
4. **Snapshot partition re-validation.** `design.md` D3, spec, task 2.1.
   - **Chose:** add `partition`/`partition_count` to `Head`. For instance items the expected values come from the manifest entry. Unpartitioned items accept whatever the snapshot carries. Replay also validates the consistency of partition entries (D2).
   - **Rationale:** otherwise an edited snapshot could upload under a different partition. This matches the existing "re-validated against the run's identity" rule.

### Skipped

_None._

## Critical

_None._

## Warning

_None._

## Suggestion

1. **Rewriting producer bytes.** `design.md` Risks. The canonical spec says the CLI retains "the producer's suite/report identities". Partition is not one of those identities, so the rewrite is consistent with it. Reviewers may still want the README to note that `suite_started.partition` is rewritten for instances.
2. **Archive ordering.** `specs/job-partitions/spec.md` REMOVES a requirement that exists only after `add-job-partitions` is archived. The proposal states this ordering. Archiving out of order would fail.

## Alignment notes

- **Other active changes:** `add-job-partitions` now MODIFIES "Reporting is explicitly configured". This change MODIFIES "Collection belongs to one invocation and job", so the two do not overlap. `generalize-test-report-filters` touches the filter check in `collect.rs`, which is independent of this change.
- **Canonical specs:** the MODIFIED requirement keeps every canonical scenario and adds three. The cap and memory rules from "Report processing is bounded and structural" are preserved without modification.
- **Codebase assumptions verified:** `create_run` creates `reports`/`pending`/`acks` per job. `JobContext` has no partition. `parse_report` copies partition from the first record. `validate_envelope` enforces agreement (`identity_mismatch`). Duplicates are discarded per `collect_dir`. `MAX_REPORTS_PER_JOB = 256`, `MAX_JOB_SOURCE_BYTES = 256 MiB` and `MAX_ENTRIES_PER_JOB = 4096` are applied per call. Replay checks `dir == path_component(name)`. The events `reports_collected`, `report_uploaded` and `report_upload_pending` exist.

## What looks good

- The `p<index>` nesting keeps the job-name encoding reversible.
- Optional manifest `partition` keeps old manifests replayable.
- A job-wide `report_id` check matches the server identity key.
- Per-instance `job_status` is preserved.
