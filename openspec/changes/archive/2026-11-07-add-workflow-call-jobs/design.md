# Design

## Context

See proposal.md for motivation and the three delta specs for the contract. This is a cross-module scheduling change, so a design is required.

`Config::validate` validates each local job DAG, while `Config::workflow` resolves one workflow and profile into a copy with the other workflow definitions removed. `Config::select` then retains its local dependency closure. `main.rs` owns a single invocation's identity, log lifetime, reporting session, history, and final summary. `runner::execute` schedules partition instances through a `JoinSet` with one shared workdir/environment and local parallelism/fail-fast state. `Output` adds workflow/profile context through clones of one serialized writer, but text lines currently print the job name, not that workflow field. `history::graph_sha256` hashes only that invocation's selected jobs, and its canonical document always includes each job's existing fields, including null command and empty steps.

These boundaries explain why a shell command invoking another goodflows process is not the implementation: it would lose coordinated concurrency and fail-fast, hide child events in command output, and make lifecycle and signal handling harder to reason about. Conversely, merging child jobs into the parent's flat namespace would break repeated names, settings ownership, and repeated-call identity.

## Goals / Non-Goals

**Goals:** Extend the existing scheduler to run an invocation tree, preserve each workflow's local DAG and runtime context, and reuse existing process execution, reporting, secure logging, history, and output paths. Make call jobs orchestration nodes, not processes occupying worker capacity.

**Non-Goals:** Template inheritance, imports, remote files, workflow-level dependencies, background triggers, invocation deduplication, a new job selector grammar, or changing existing command/step execution. Workflow inputs, outputs, call-level env, partitions, and subtree timeouts are deferred rather than partially implemented.

## Decisions

### 1. Extend Job directly and validate execution forms at the source

Add an optional `workflow` target to `Job`, omitted when absent during serialization. Extend `validate_job` to accept exactly one execution form while keeping existing command/step behavior. Call jobs retain local dependencies and conditions, but reject execution-only modifiers specified in the workflow-calls spec. An empty `steps` default is not a fourth execution form, and default/empty collection values do not create spurious conflicts. Conditions on calls resolve in the caller's effective environment and can read outputs of ordinary local prerequisites; a workflow call itself exposes no outputs.

Generalize the existing validation knowledge about output-ineligible dependencies to cover calls as well as partitions. Do not turn absent call outputs into null expressions: reject references before execution, just as partitioned output references are rejected today.

Validate every workflow reference, then traverse a separate workflow-call graph for direct and indirect recursion. Include implicit root `default` and identify call-job edges in diagnostics. Use an iterative traversal like the existing job cycle diagnostic so deep graphs are not rejected by a small arbitrary nesting limit or allowed to overflow a recursive validator. All authored edges participate, even if a call is not selected or has a false condition.

Alternative: a new top-level call/template registry would duplicate the existing workflow namespace without improving this same-file feature.

### 2. Resolve a reachable execution plan before runtime side effects

Keep the loaded document available instead of passing only the workflow-stripped copy to execution. Resolve the root with existing settings/profile APIs, preserve the existing root profile checks, apply root `--jobs`, and traverse calls reachable from that selection. Resolve each target with the same explicit profile and applicable CLI overrides. A called workflow always selects its complete local graph. Validate selected-profile compatibility in the reachable graph before creating sessions or starting root jobs, even when a call condition will later skip activation.

Settings resolution is not "the caller's workflow settings never apply." `SettingsOverrides::apply` merges shared settings with the target workflow per key: an omitted child key inherits the shared value, and only an explicit child override replaces it. Profile settings then overlay that result. Env follows the same per-key merge. Do not copy the caller's resolved workflow settings as the child's base, and do not treat an omitted child setting as empty when shared settings define it.

Profile partition maps stay document-global and name-based. `Config::validate` already requires every profile entry to name a job that declares `partitions` in some workflow, and `Config::workflow` fails when the selected workflow has an unpartitioned job of that name. Reachable-call checks must use that same rule: a selected profile that is incompatible with any reachable workflow fails before root side effects, including when the call condition would be false. Do not invent per-workflow partition namespaces. Two workflows that both partition a job named `test` share one profile count; a workflow whose `test` is not partitioned remains incompatible with a profile that sets `test`.

A workflow that calls itself is a direct cycle. Reject it with the same diagnostic as any other cycle. Do not add a special self-call, recursion depth, or re-entry mode.

Resolved workflow definitions can be reused as immutable plan data; runtime state cannot. Allocate each child invocation only when its call passes dependencies and its condition, with a fresh identity for each activation. Repeated calls after a mutation therefore see the current filesystem. No snapshots or automatic workspace isolation are promised: concurrent workflows share the filesystem just as concurrent jobs do today.

List/validate inspect this plan without activating invocations. Their job counts stay local configured-job counts, call rows expose `workflow`, and discovery keeps its existing per-workflow structure. Aggregate validation still checks all workflow/profile combinations. Do not eagerly expand repeated calls into an exponentially sized runtime tree before execution.

### 3. One scheduler coordinates local DAGs and hierarchical capacity

Extend the scheduling ownership in `runner.rs` to maintain active invocation states, each with its local job/partition results, context, effective settings, and optional parent call reference. Reuse the existing command/step execution path for executable units. A ready call evaluates its condition once, activates a child, and remains pending until that child finalizes; it is not submitted as a worker task waiting on another worker.

Maintain a root-wide capacity count and an executing-descendant count for each active invocation. Launch a command/step job instance only when the root and all ancestor subtree limits permit it; increment/decrement those counts together. A call's shell condition uses a temporary executable unit under the same checks and releases capacity before child activation. Expression evaluation and a waiting call use no slot. Preserve existing configuration/partition launch ordering within an invocation; cross-invocation launch order is not a new public guarantee, but ready work must make progress when capacity is available.

This avoids both single-slot deadlock and capacity hoarding by tasks that acquire one semaphore and then wait for another. CLI parallelism applies through normal settings precedence to every invocation; the root remains the total ceiling, and a child's lower effective limit constrains its subtree rather than reserving capacity away from siblings.

The capacity count is of command/step job instances and temporary call-condition shells, not of every process. A stepped job is one instance even while its steps run sequentially, matching today's one-slot-per-instance scheduler. A call's own shell condition is a temporary unit: it occupies a slot only while that shell runs, then releases it before the child starts. It is not a second permanent instance of the call job.

Keep one RSS sampler for the whole tree. `RssMonitor` is one task per `execute` today; nested or per-invocation samplers would multiply `/proc` scans. Register every spawned process group, including call-condition shells, with that shared sampler, and still attribute each measurement only to the invocation that spawned it.

Alternative: independent recursive `execute` calls with independent limits oversubscribe the machine. Adding nested permit acquisition around that recursion makes fairness and deadlocks harder to control than accounting at the existing launch boundary.

### 4. Propagate policies without changing process ownership

Observe every executable failure in its owning invocation and each ancestor. An enabled fail-fast policy stops future starts throughout that invocation's subtree, even while the failed child's other jobs settle. A disabled child cannot opt out of an enabled ancestor. A locally enabled child policy does not stop independent branches under a disabled parent. Already executing command/step jobs finish; dependency-skip reasons keep their existing precedence once prerequisite instances settle.

Keep one root signal receiver shared by the tree and the existing process-group cancellation/reaping machinery for executable units. No child CLI process or extra signal handler is needed. Cancellation prevents child activation, cancels pending/active descendants, and preserves the root's 130/143 exit behavior, including failure-versus-cancellation outcome precedence.

A completed child maps its workflow outcome to the call: passed -> passed, failed (including a timeout) -> failed, cancelled -> cancelled, except that a passed child with a local job or call skipped by fail-fast was interrupted by an enclosing fail-fast stop, and its call is skipped with the fail-fast reason (see the post-review note below). Use the existing `history::outcome` rule for that mapping, including the one signal snapshot: a child with no failed or timed-out instance is cancelled when the snapshot is set, even if every local instance passed. Ordinary workflow success already allows conditional skips, so calls must not require every child job to pass; doing so would make optional child checks unexpectedly block release. A false condition on the call itself still produces a skipped call and blocks its dependents. Call preparation/activation errors become contextual call failures, not unhandled errors that prevent a root summary.

No synthetic exit code or shell command is assigned to a call. Its `duration_ms` covers its own condition and the complete child lifecycle, excluding dependency wait, and is the same integer already stored on `JobResult` (`u128` milliseconds). Text rendering reads that number as a JSON integer; do not introduce a second duration type that `as_u64` cannot read. Own shell-condition costs are retained; descendant CPU/RSS measurements are not copied into wrapper costs. Each invocation has its own execution-start instant: `blocked_ms` measures dependency wait from that instant, and `queued_ms` measures subsequent capacity wait. Child metrics exclude earlier parent preparation and call-condition evaluation; a dependency-free child job has `blocked_ms` near zero even when the parent spent time preparing. The parent call's own wait metrics use the parent's start instant. A skipped call that never started still has no row duration.

### 5. Reuse per-invocation lifecycle inside the coordinated execution

Extract the invocation lifecycle currently in `main.rs` only where necessary for root and child reuse: identity, effective context, secure log creation/lock lifetime, reporting session, source/host capture, final report collection, history, and summary. Keep CLI parsing and inspection at the root. The scheduler must be able to activate/finalize an invocation without blocking progress in sibling invocations; filesystem/network finalization can run asynchronously while command capacity remains available elsewhere.

Each activated child gets a distinct existing `RunIdentity`, even when history and reporting are off. Child logs use the existing config-workflow namespace and unique run-stem directory; root legacy logging remains flat as today. Hold each log lock until that invocation finalizes and preserve retention/symlink rules. A command-free call has no synthetic log pair; a call shell condition can write its own ordinary log pair. A failure creating child run state fails its parent call; normal reporting and history write warnings retain their existing nonfatal rules.

Each child gets its own reporting session and report directories, using the global reporting configuration, target workflow name, selected profile, and its own workdir/source. Only report-producing command/step jobs provide reports. Wrapper jobs are not producers. Existing server envelopes and replay identities already model independent workflow invocations, so do not add unsupported parent metadata to the report wire format. Preserve credential scrubbing and runner-owned reserved variables through the current environment-building path.

Child finalization includes ordinary bounded report collection/publishing and its summary, then resolves the parent call. All children settle before root finalization, guaranteeing the root report remains last.

### 6. Add invocation context without flattening result ownership

For a root selection containing reachable call jobs, attach `invocation_id` to all runtime events from the start, including the root. For child events also attach `parent_run_id` and `caller_job` using `Output`'s existing contextual-clone pattern and serialized writer. `invocation_id` is the owning invocation UUID, equal to its eventual summary `run_id`; it is distinct from adding optimizer measurements to live events. No-call root selections retain existing event shapes. Child setup errors carry the child context when its identity has been allocated. These fields are added to the event object; they do not replace `workflow`, `name`, or partition identity.

Text mode currently prints the job name, not the event's `workflow` field. Adding JSON context is not enough for the live-text requirement. When calls are present, live job/step lines and streamed chunks must include the owning invocation's workflow name and a short invocation distinguisher (the invocation id's first 8 hex characters is enough). Repeated calls to the same target then stay distinguishable without a new selector syntax. No-call runs keep today's labels. `list` text must also print a call job's target; today it prints `command`, which is absent for a call.

Add optional target `workflow` and `child_run_id` fields to call results. Include the target even for a skipped call, but only include `child_run_id` when activation allocated an identity. The enclosing event's `workflow` still identifies the owner, not the target. History uses the same explicit exception to its instance allowlist and adds parent/caller identity to child records; it does not store resolved paths or env values.

Each summary counts only its invocation's local instances, with each call one instance. Text reports show the target and child identity on the call row and give child reports a caller/parent label. Child failures retain actual job/step details in child reports; the root wrapper points to the child. Live text lines and streamed chunks must also distinguish invocation context when calls are present, so concurrent same-named child jobs are not ambiguous. Preserve escaping of control characters in all new labels. Root rerun hints select local call names through the existing safe quoting/fallback implementation, so preparation dependencies are not bypassed.

### 7. Fingerprint resolved child graphs but preserve old hashes

Extend the canonical graph job representation with an optional workflow-call object, omitted entirely for ordinary jobs. When present it contains `workflow` (the target name) and `graph_sha256` (the target's own graph hash over its complete jobs under the selected profile, the same value a child invocation of that target reports). Do not add the target name as a sibling of `command` on ordinary jobs, and do not omit existing fields on call jobs: `command` stays null, `steps` stays empty, and `shell`, `depends_on`, `timeout_secs`, and `condition` stay in their current positions. Serde field order is the hash contract; inserting a field ahead of an existing one, or dropping a null/empty field, changes no-call hashes. Do not use runtime identities in the graph document. Root filtering determines reachability, while call conditions do not remove child graphs from the fingerprint. A call that is selected but whose condition is false still contributes its resolved target graph.

Hash each reachable workflow once per invocation and reuse that hash for every call path that reaches it, like a Merkle tree; executions are still not deduplicated. The work is linear in the configured jobs, so repeated calls at several nesting levels cannot blow up the canonical document before any job runs (see the post-review note below). Child commands, nested call targets, and effective partitions must influence ancestor hashes; env values, resolved paths, and unrelated workflows must not. Keep each invocation's hash rooted at its own selection.

Track `max_in_flight` over executable units in each invocation's subtree, excluding wrappers and including temporary call shell conditions. Child cost measurements remain local so tooling cannot accidentally sum duplicated process measurements from parent wrappers.

### Post-review decision: interrupted children skip their call

After review, the user decided that a call whose child was cut short by an ancestor's fail-fast shows Skipped with `fail_fast: previous job failed` instead of Passed, and authorized the spec change. The child is interrupted when the existing `history::outcome` rule gives passed but one of its local results was skipped with the fail-fast reason; failed and cancelled outcomes take precedence. The child's own summary and history outcome are unchanged, the call keeps `workflow`, `child_run_id`, and `duration_ms`, and the rule composes through nested calls. Children that pass with ordinary condition or dependency skips still pass their call. Configurations without calls are unaffected.

### Post-review decision: calls embed their target's hash

PR review found that embedding each target's complete canonical jobs, recursively, repeats a target once per call path. Twenty-five levels that each call the next twice produce tens of millions of job copies from about 50 configured jobs, and the hash is computed before any condition runs, even with history off, so such a config could exhaust memory before starting. The user approved changing the call object to carry the target's own `graph_sha256`, computed once per workflow and reused. Hashes of selections without calls are unchanged; only selections with calls, which are new in this change, hash differently from the first implementation. A command change in any reachable workflow, including behind a false call condition, still changes the root hash.

## Risks / Trade-offs

- [Scheduler change affects every existing job] -> Preserve the current execution path for leaf jobs and run existing scheduling, partition, process cleanup, and fail-fast tests unchanged alongside asymmetric call cases.
- [A waiting call holds capacity or sibling calls oversubscribe] -> Admit executable work at one launch boundary; prove nested single-slot progress and measure simultaneous instances across sibling invocations and lower child limits.
- [Settings or root selection leak into children] -> Resolve from the original document, use different caller/child workdirs and conflicting env values in tests, and test a root selector absent from the child.
- [Repeated calls duplicate side effects] -> Make this explicit and documented; users order calls through dependencies when filesystem mutations must not overlap. There is no automatic caching or isolation.
- [Child lifecycle work stalls unrelated jobs] -> Keep finalization asynchronous and release executable capacity before reporting; test a delayed mock upload alongside independent work.
- [Nested failures produce misleading summaries or unsafe reruns] -> Keep local results local, identify child failures, execute generated root hints in integration tests, and retain quoting/comma fallback coverage.
- [New metadata breaks agents expecting one summary] -> Document one summary per invocation and root-last ordering; preserve no-call shapes and ensure runtime identities are present before any child activates.
- [History/profile combinations diverge across invocations] -> Test child-only history, global `--no-history`, and child-specific profile incompatibility before any root side effects. Profile incompatibility includes an unpartitioned reachable job whose name appears in the selected profile's partition map.
- [Call-aware events change no-call JSON] -> Attach invocation fields only when the selected reachable graph contains a call, and keep no-call event key sets identical. `tests/output.rs` asserts exact key lists for `job_started`, `step_started`, `step_finished`, and `job_output`.
- [A shared RSS sampler is dropped or duplicated] -> One sampler remains for the tree; child commands and call-condition shells still record peaks, and wrapper history does not copy those peaks.

## Migration Plan

No existing config or stored record is rewritten. Existing configurations without workflow calls keep their semantics, shapes, paths, and graph hashes. README adds the new execution form, a preparation/call/publication example, the explicit dependency requirement, separate-call behavior, settings and concurrency rules, self-call rejection, and child-summary identities. Document that profile partition entries remain global by job name, so a reusable workflow must not reuse a partitioned job name that another workflow leaves unpartitioned under a profile that sets that name. Older binaries reject the new `workflow` job field through `deny_unknown_fields`; rolling back requires removing or replacing call jobs. This change does not publish a binary, push code, or perform a release.
