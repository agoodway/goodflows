# Running workflows on Sprites

`goodflows run --sprite <identity>` runs a complete workflow on a
[Sprite](https://sprites.dev) that belongs to one caller identity of one
project. GoodFlows creates or reuses that Sprite, delivers a verified snapshot
of your working tree, runs your explicit preparation hook, executes the
unchanged workflow with the same GoodFlows version, and brings the results
back. Without `--sprite`, nothing changes: runs stay local and never call the
Sprite CLI.

> **Support status.** Local behavior, configuration, preflight, snapshotting,
> ownership, locking, recovery, collection and destruction are covered by
> contract tests against a fake Sprite CLI. Live acceptance on disposable
> Sprites in a platform-verified organization covered controller-kill
> survival, worker, supervisor and service loss, the fault matrix and the
> dogfood comparison. **Recovery after an actual cold boot or cold wake (a
> changed boot identity) was not exercised and is unverified and unsupported**,
> pending future validation. Unchanged-boot service restarts, suspend/resume
> and checkpoint/restore are not cold boot and do not count as evidence of it.
> Treat VM loss as `interrupted`, never as resumable.

## Requirements

- The official Sprite CLI on `PATH`, authenticated (`sprite login`, or
  `sprite auth setup --token ...` in automation). Verified against
  `sprite version 2026-11-03 (4eb1509)`.
- Git: the snapshot is taken from the Git working tree containing the config.
- A Linux runner for the Sprite's architecture. A Linux controller of the same
  architecture installs itself; other controllers (for example macOS)
  download the exact version's checksummed Linux release
  (`goodflows-v<version>-<arch>-unknown-linux-gnu.tar.gz`). If a Linux
  controller's own executable is installed but cannot run on the Sprite (for
  example it needs a newer glibc than the Sprite image has) while the Sprite
  still runs a plain command, GoodFlows emits a `sprite_runner_fallback`
  event (the reason, the release runner id, and whether it came from the
  cache) and installs the exact version's release instead. The release, once
  its published checksum is verified, is cached on the controller in
  `.goodflows/sprites/runners/v<version>-<arch>/` under the Git root of the
  selected config (one cache per project, whichever directory you run from)
  together with its digest.
  That digest only detects a damaged copy (it is stored next to the binary,
  so it proves nothing about who wrote it): a cached copy is integrity-checked
  against it and used only while the directory is private to you (owned by
  your user, mode 0700), both files are yours and not writable by others, and
  Git tracks nothing there. Otherwise the release is downloaded and its
  checksum verified again (a non-private cache directory is not rewritten).
  Before falling back, GoodFlows asks an already installed copy of its own
  build a second time, so one failed call never switches runners. A runner
  already installed on the Sprite with the same content is never uploaded
  again, so later runs from such a controller transfer nothing extra. The
  fallback runs the *published* build of the same version: GoodFlows has no
  marker that tells a released executable from a local build of unreleased
  changes with the same version, so a development build that falls back runs
  the release's code on the Sprite, not its own; the event says so. Use a
  build that runs on the Sprite (or an official release) when you test
  unreleased changes. An unreleased version without a matching Linux artifact
  fails before any hook or job. GoodFlows never uses `latest` or a `goodflows`
  already on the Sprite.

No Spry binary or library is needed.

## Sprite CLI contract

Recorded from `sprite --help`/`<command> --help` of the official CLI
`sprite version 2026-11-03 (4eb1509)` (installer `https://sprites.dev/install.sh`,
`linux-x86_64` binary SHA-256
`731af477ecf7328fd188c67b3649874c9107b6f0097c25888d0dd17aad823527`, matching the
release manifest) and from the [Sprites API](https://docs.sprites.dev/api/dev-latest/sprites/)
and [Tasks API](https://docs.sprites.dev/keeping-sprites-running) documentation.
`sprite version` is not a command; use `sprite --version`.

| Concern | What GoodFlows relies on | Evidence |
|---|---|---|
| Organization selection | Every call passes `-o <org>`; the org is resolved once from `SPRITE_ORG`, else the `(current)` entry of `sprite org list`. `-o` is **not** proof of where the CLI acts: before any receipt, create, exec or destroy, and before reading status, logs or results, GoodFlows requires the listing's top-level `org.name` and every listed Sprite's `organization` to equal the requested org, and otherwise refuses with exit 1 naming both orgs | global flag `-o, --org`; `SPRITE_ORG` documented as an override; `org list` marks the default. The CLI reference describes `SPRITE_TOKEN` as an “API token override” and tokens have the form `org-slug/org-id/token-id/token-value`; the API lists Sprites “for the authenticated organization”. Observed live (2026-11-07): `sprite list -o goodway --json` under a `SPRITE_TOKEN` for another org reported `"org":{"name":"thomas-brewer",…}` and `"organization":"thomas-brewer"` on every Sprite, with no warning. Static inspection of the installed binary finds a `SPRITE_TOKEN` override but no `SPRITES_TOKEN` string, so `SPRITES_TOKEN` alone does not appear to select the CLI's credential |
| Sprite selection | `exec -s <name>`; `create`, `destroy`, `info` take the name positionally. Never `sprite use`, which writes a `.sprite` file | `exec --help`, `use --help` |
| Creation | `sprite create -o <org> --skip-console <name>`; only a zero exit of this call counts as “we created it” | `create --help`; API `POST /v1/sprites` returns 201, and **400 for an invalid body or an existing name**, so a conflict is indistinguishable from other bad requests and is never treated as success |
| Stable identifier | `sprite list -o <org> --prefix <name> --json`, field `id` when present | API create/get responses carry `id`; the list response is documented inconsistently (`name`, `org_slug`, `updated_at` in one version). Destroy fails closed: it refuses unless the listed `id`, the id recorded at creation and the id seen at lookup are all present and equal, and any listing with more than one Sprite of the name is refused. Verified live (2026-11-07, org list of real Sprites): `{"org":…,"sprites":[{"created_at","id","name","organization","status","updated_at","url","url_settings"}]}` with `id` of the form `sprite-<uuid>`; a listing without an id is still refused |
| Streaming stdin | `exec --stdin` (WebSocket, non-TTY) for the archive; `--no-stdin` for everything else | `exec --help`: `--stdin`, `--no-stdin`, `--http-post` (HTTP/1.1, 10 MiB stdin cap per API docs) |
| Port forwarding | `--no-port-forward` on every exec | `exec --help` |
| Session lifetime | Not relied on: non-TTY sessions suspend on detach, and a plain exec stopped about 10 s after its controller was killed in the lifecycle probe | `sessions --help`; probe evidence |
| Durable work | `sprite-env services create <name> --cmd ... --args a,b --dir ... --no-stream` inside the Sprite; Tasks API `PUT /v1/tasks/<name>` (upsert, max 1 h, returns 200), `DELETE` (204/404) over `/.sprite/api.sock`, host `sprite` | Tasks API docs; probe evidence (sprite-env 0.0.4) |
| Destruction | `sprite destroy -o <org> --force <name>` after GoodFlows' own confirmation | `destroy --help` (`--force` skips its prompt) |
| Names | `<prefix>-<32 hex>`, 3–63 lowercase letters, digits and hyphens, alphanumeric at both ends | Sprites staff answer on the Fly.io community forum (no formal reference found) |
| Credentials | `SPRITE_TOKEN`/`SPRITES_TOKEN` stay in the CLI's environment or keyring; never in argv | `auth setup --help` takes `--token` in argv, so GoodFlows never calls it |

## Configuration

```toml
[settings]
sprite_prefix = "goodflows"   # project-wide; required for every Sprite operation

[sprite]
setup = "bash scripts/sprite-setup.sh"    # runs until it succeeds once
resume = "bash scripts/sprite-resume.sh"  # runs on every later prepared run
pass_env = ["DATABASE_URL"]               # caller variables forwarded (default: none)
```

- `sprite_prefix`: 1–20 lowercase ASCII letters, digits or hyphens, starting
  with a letter or digit. Only valid in the top-level `[settings]`; a
  workflow or profile that sets it fails validation.
- `[sprite]` is top-level only. `setup` and `resume` must be nonempty shell
  commands when you run remotely; write `true` for a deliberate no-op.
  GoodFlows never discovers scripts (an `.agents/setup` file is ignored) and
  never infers toolchains or dependencies.
- `pass_env` lists unique variable names. Unset names fail before anything
  remote happens; an empty value counts as set.
- Partial configurations stay valid for `validate` and `list`, which are
  offline: they need no Sprite CLI, credentials or allowlisted values and show
  names, never values.

`--env` keeps its meaning: it selects a workflow profile, never a location.

## Command line

```text
goodflows run [workflow] --sprite <identity> [existing run flags]
goodflows sprite status  <identity> [--json]
goodflows sprite logs    <identity> --run <uuid> [--json]
goodflows sprite collect <identity> --run <uuid> [--output <directory>] [--json]
goodflows sprite cancel  <identity> --run <uuid> [--json]
goodflows sprite destroy <identity> [--yes]
```

The identity is opaque, case-sensitive caller data (1–256 UTF-8 bytes, no
control characters), such as an agent thread id. It is never used as a
Sprite name or shell text. The `sprite` subcommands accept `--config` and
`--json` and reject the run-only flags (`--env`, `--jobs`, `--parallelism`,
`--log-dir`, history flags) instead of ignoring them. They need only the
project prefix: no current source, hooks or run-only variables.

### Names, ownership and sharing

Within the organization, the prefix and identity select one Sprite named
`<prefix>-<32 hex>`: a SHA-256 over a versioned, length-delimited tuple, so
case or punctuation differences never merge identities (at most 53 bytes,
within the platform's 3–63 lowercase/digit/hyphen rule). The Sprite stores a
private ownership record (`~/.goodflows/owner.json`) with the organization,
prefix, exact identity and digests; every operation verifies it.

- Projects that must not share environments must use different prefixes.
  Projects with the same prefix in the same organization share identities.
- A Sprite with the computed name but no matching record is never adopted,
  prepared, overwritten or destroyed. A crash between creation and
  initialization, or a lost create acknowledgement, leaves such an unowned
  Sprite for manual reconciliation (`sprite destroy -o <org> <name>` after you
  have checked it). So does a new Sprite that `sprite list` does not report
  with its resource id within 10 listings a second apart: the ownership
  record must hold that id, because destruction verifies it, so the run is
  refused (exit 1) without initializing the Sprite.
- Ownership is a consistency check, not an authorization boundary: other
  organization administrators and jobs running as the same user can change it.

The organization is `SPRITE_ORG` when set, otherwise the Sprite CLI's current
default organization (`sprite org list`). Every call names the organization
and Sprite explicitly; GoodFlows never runs `sprite use` or changes the active
context, never exposes URLs and disables port forwarding.

Naming the organization does not choose it: the Sprite CLI acts in the
organization its credential belongs to (`SPRITE_TOKEN` when set, otherwise the
stored token), and a listing under `-o <other>` can silently describe the
credential's own organization instead. Every remote command therefore lists
the identity's Sprite first and verifies that the platform-reported
organization (`org.name`, and `organization` on each listed Sprite) is present
and equal to the requested one. On a mismatch or a missing field it stops
with exit 1 before any receipt, create, exec or destroy, and before reading
another organization's status, logs or results. The diagnostic names both
organizations, never a credential value. Fix it by using a credential for the
intended organization, or by setting `SPRITE_ORG` to the credential's
organization when that is the intended target.

### One run per identity

A remote, process-held lock per organization, prefix and identity, separate
from run-UUID idempotency, admits one run at a time across controllers,
workflows and profiles. It covers upload, preparation, execution, descendant
cleanup and durable finalization: the submitting process hands the locked
descriptor to the supervisor, which passes it to the run's worker at exec, so
the lock is never free in between (hooks and jobs do not inherit it). The
durable active-run record and process-liveness checks still apply as a second
line of defense. A contender, including one whose controller uses a different
GoodFlows build, fails at once with the active run's UUID and creates no run
record. Before installing a different runner, such a contender asks the
installed runner whether a run is live and stops without installing anything.
That check is advisory: on first use, or when the installed runner cannot
answer, the new build is installed first and admission reports the conflict:

```text
Error: Sprite identity T-1 is busy with run 6d1c...; it was not modified (check `goodflows sprite status`)
```

Disconnecting does not free the identity. Only a durably finalized run, or a
run reconciled as `interrupted` after every recorded process is proven gone,
frees it. Submitting a recorded UUID again only looks it up; it never reruns.
Different identities never serialize each other.

## What is sent

The snapshot is the current working tree of the Git repository containing
the config: tracked files with their current contents (not `HEAD`) and
nonignored untracked files, from
`git ls-files --cached --others --exclude-standard`. Deleted files are absent;
ignored files (build output, caches) are left out by your own ignore rules.
Files are copied once into a private, read-only staging directory, and the
manifest (paths, sizes, SHA-256, executable bit) is computed from those
copies. A file that changes during the copy fails the run.

Always excluded, with a path and reason shown (never contents): `.git`,
`.amp`, `.sprite`, `.goodflows`, `.env` and `.env.*`, `.netrc`, `.npmrc`,
`.pypirc`, `.git-credentials`, `credentials*`, `secrets.*`, private-key files
(`id_rsa`, `id_ed25519`, `*.pem`, `*.key`, `*.p12`, `*.pfx`, `*.jks`,
`*.keystore`), anything under `.ssh`, `.aws` or `.gnupg`, files containing a
recognized private key, and files containing the value of a controller
credential (`SPRITE_TOKEN`, `SPRITES_TOKEN`, the reporting `token_env`) or of a
forwarded variable at least 8 bytes long. This is a conservative heuristic,
not a secret scanner; synthetic test keys are excluded too.

Rejected before anything remote: special files, submodules (gitlinks),
unresolved, cyclic or external symlinks (their targets are never read),
file/directory path collisions, a config or path-based hook script that is
not in the snapshot, any declared workdir outside the Git root (including
unselected workflows and profiles), and non-Git directories. Internal
symlinks are delivered as copies of the in-root file they name.

Workflows that need `.git` (for example `git describe`) or absolute
controller paths in command text do not work remotely; GoodFlows does not
rewrite your commands.

The archive is one gzip tar streamed over the CLI's WebSocket stdin
(`sprite exec --stdin`), never the 10 MiB-bounded HTTP POST mode. It is
extracted into a fresh `runs/<uuid>/source.incoming/`, verified against the
manifest (inventory, hashes, sizes, modes) and only then renamed to
`source/`. An interrupted or mismatched transfer leaves no runnable tree and
never touches earlier runs.

## Preparation, state and environment

After source activation exactly one hook runs (`sh -c`, null stdin) in the
selected workflow's workdir: `setup` until it has succeeded once, `resume`
afterwards. Only a successful setup is recorded as prepared. A failed setup is
retried next time; a failed resume does not clear preparation; a lost setup
result is treated as unprepared. Changing the hook text does not silently
re-run setup: the run reports a `sprite_setup_changed` event and still runs
`resume`; use `resume` for migrations, or destroy and recreate an idle
Sprite. Either hook's failure starts no job (exit 1).

Hooks and jobs get `GOODFLOWS_SPRITE_STATE_DIR`, an absolute directory outside
the per-run source that persists for the identity. Your scripts own caches
and toolchains there and link them into each fresh source tree; GoodFlows
never discovers, mounts or copies caches. Every process a hook starts is
killed when the hook ends, so a hook cannot leave a server running for the
jobs: start long-lived services with `sprite-env services` (they outlive the
run, and you own their lifecycle) or inside the job that needs them. The variable is reserved:
configured env and `pass_env` cannot set it, and local runs remove any
inherited value.

A hook's `export` does not reach later commands; there is no shared shell
session. Use configured env, fixed executable paths or scripts. Login-shell
`PATH` changes are not assumed to persist.

Environment: hooks and jobs start from a minimal base (`HOME`, `USER`,
`LOGNAME`, `PATH`, locale, `TZ`) plus forwarded values and runner-owned
variables. Nothing else from the controller is forwarded. For jobs the
forwarded values are the lowest-precedence caller layer, under settings →
workflow → profile → job → step env. Hooks receive the exact forwarded values.

Remote runs deny, in `pass_env` and in every configured env layer:
`SPRITE_TOKEN`, `SPRITES_TOKEN`, every reporting `token_env`, and the
runner-owned `GOODFLOWS_RUN_ID`, `GOODFLOWS_REPORT_DIR`,
`GOODFLOWS_PARTITION`, `GOODFLOWS_PARTITION_COUNT`, `GOODFLOWS_ENVIRONMENT`,
`GOODFLOWS_OUTPUT` and `GOODFLOWS_SPRITE_STATE_DIR`. A reporting `url_env` can
be forwarded explicitly. Forwarded values travel as a separate private input
frame to the supervisor and worker processes, never in argv, archives,
metadata or diagnostics, and are never written to disk by GoodFlows. Hooks
you give a secret can still print or store it; raw output is not redacted.

Trusted setup/resume scripts may use privileged installation where the Sprite
allows it. The workflow runner starts with all capability sets dropped
(ambient, bounding — or, when the bounding set cannot be dropped,
`no_new_privs` — effective, permitted and inheritable), verified before
launch; if that cannot be established the run ends as `preparation_failed`
(exit 1, no job started) rather than skipping permission-sensitive tests. A reused Sprite with its caches and services is
one trust domain: fresh source directories do not isolate hostile jobs.

## Execution and recovery

The controller hands the accepted run to a Sprite-managed service,
`goodflows-supervisor` (created with `sprite-env services create`). The
service starts a worker per run, which registers its boot id, PID, start time
and execution generation before the supervisor lets it start, holds the
caller lock it inherited from the submission, renews a Tasks API wake hold (`PUT /v1/tasks/gf-<uuid>` with a
12 s expiry about every 3 s), runs the hook and the workflow runner, kills
remaining descendants, flushes results and writes the terminal record before
it frees the identity and deletes the hold. A plain `sprite exec` stops about
10 s after its controller dies, so it never owns execution. Service creation
is checked by protocol readiness and the expected new generation, not by the
CLI's exit status.

If the worker itself hits an error after starting a hook or the workflow
runner, it kills every process left in its session before it records
`interrupted` and frees the identity; if anything survives that, it leaves the
active pointer and the nonterminal record in place (the worker itself exits),
so reconciliation keeps the identity busy until those processes are gone.

If an identity stays busy for good (a recorded process that never dies, or a
Sprite whose boot id cannot be read), `goodflows sprite destroy` refuses too,
because it never deletes under an active or unresolved run. The last resort is
the Sprite CLI itself, after you have checked the Sprite:
`sprite destroy -o <org> --force <name>` (the name is in `sprite status`). It
deletes the environment and every uncollected result.

If renewal fails, admission is blocked at once and the run is stopped before
the hold can expire; the identity stays held until the run is finalized. If
the worker dies before it finalizes, the reconciliation that records the run
as `interrupted` (in `sprite status`, admission or supervisor startup) also
removes that run's admission block. Renewal stops as soon as the terminal
record is durable (the hold itself is still deleted last), and reconciliation
removes a block left by a run whose record is already terminal.
Hold expiry is never treated as proof that work stopped.

Phases: `accepted`, `uploading`, `verifying`, `setup` or `resume`, `running`,
`finalizing`, then one of `completed` (with the workflow's own exit code),
`preparation_failed`, `transfer_failed`, `cancelled` or `interrupted`.
`unreachable` is only a controller observation.

Recovery from any controller with the same config prefix:

```bash
goodflows sprite status T-1                    # ownership, preparation, active and recent runs
goodflows sprite logs T-1 --run <uuid>         # saved lifecycle and workflow events
goodflows sprite collect T-1 --run <uuid>      # verified local bundle; never publishes
goodflows sprite cancel T-1 --run <uuid>       # explicit cancellation of that run only
```

`status` may record `interrupted` after reconciling under the caller lock:
only when the worker, the workflow runner and every recorded session are
proven gone (boot id, PID start time and generation checks). A surviving
descendant keeps the identity blocked. `logs` and `collect` only read. None of
these commands transfers source, runs hooks or reruns jobs. Startup of the
supervisor service reconciles the same way before it accepts work and never
relaunches recorded runs.

An acknowledged `cancel` stops a running hook or workflow. One acknowledged
before the hook or the workflow starts (during upload, verification or
handoff, say) ends the run `cancelled` without starting it; a Sprite whose
setup never ran stays unprepared, so the next run runs setup.

Example `sprite status --json` event (nonsecret fields only):

```json
{"type":"sprite_status","identity":"T-1","sprite":"goodflows-0f3c...","state":"active",
 "message":"Sprite goodflows-0f3c... (identity T-1)\n  prepared: yes\n  active run: 6d1c...",
 "busy":true,"active_run_id":"6d1c...","prepared":{"run_id":"2a9e...","setup_digest":"...","prepared_at_ms":1762500000000},
 "degraded":false,"destroying":false,"reconciled":true,
 "runs":[{"run_id":"6d1c...","phase":"running","workflow":"ci","environment":null,"accepted_at_ms":1762500100000,"terminal":null,"timings":[...],"path":"/home/sprite/.goodflows/runs/6d1c..."}],
 "owner":{"organization":"my-org","prefix":"goodflows","identity":"T-1","name":"goodflows-0f3c...","sprite_id":"..."},
 "supervisor":{"protocol":1,"generation":"...","runner":"...","version":"0.7.0"},"schema_version":1}
```

Limits of reconciliation: platform or VM loss is reported as `interrupted`
with partial results, never restarted automatically. Every process of a run
stays in the worker's session (hooks and the workflow runner get their own
process groups inside it), so an unregistered descendant still blocks the
identity; a descendant that leaves the session (`setsid`, daemonizing) cannot
be tracked. Measured on a Sprite: killing only the supervisor process leaves
the worker running and the run completes; killing the service
(`sprite-env services signal ... KILL`) also kills its workers, and the
restarted supervisor records `interrupted` at startup. All of these kept the
same boot id. If the Sprite's boot id cannot be read, no recorded process is
treated as gone and the identity stays busy. Recovery after an actual cold
boot or cold wake (a VM restart
with a new boot id) is unverified and unsupported, pending future validation:
it was not exercised. Unchanged-boot service restarts, suspend/resume
(including a Sprite that `sprite list` reports as "cold" but that resumes with
the same boot id) and checkpoint/restore are not cold boot and are not
evidence of it. GoodFlows still fails closed: a recorded worker from an
earlier boot is treated as gone, so reconciliation records `interrupted` and
never replays the run.

### Cancel versus disconnect

Ctrl-C (SIGINT/SIGTERM) on an attached run sends a UUID-bound cancellation to
the supervisor, which stops the hook's process group or signals the remote
GoodFlows runner so the existing cancellation logic finalizes the jobs. A
closed terminal, a broken pipe or a lost connection does **not** cancel: the
run continues. An attached controller reconnects with backoff (1 s, doubling
to 15 s). A reconnect asks the runner to leave out the lines already
printed (`follow --skip <lines>`, acknowledged by the runner); a runner from
before that option ignores it and replays the whole saved stream, which the
controller then skips line by line, so nothing is printed twice either way.
A connection counts as working only if it delivers new output, or delivers
something (the acknowledgement or replayed lines) and stays up for 30 s; a
connection that never delivers a byte does not count, however long it stays
open. Losing a working connection restarts the 5-minute budget, so a long
quiet job whose stream drops still gets a full budget. Connections that only
replay and drop, or that stall without delivering anything, do not: after 5
minutes without a working connection the controller exits 3 with the run
reference. If a
cancellation is not acknowledged and no terminal result is retrieved, the exit
code is 3 and the identity stays held until the run finalizes or is
reconciled. Cancelling a finished run signals nothing.

Ctrl-C before the run is submitted (snapshot, provisioning, runner install)
stops at the next safe point, after the Sprite CLI call in progress, submits
nothing and exits 130 (143 for SIGTERM); it never interrupts a create or an
install half way. `sprite status`, `logs`, `collect` and `cancel` stop at once
with the same exit codes: they only read or make an idempotent request, and a
collection stopped midway leaves either nothing or a complete verified bundle
(publication is a single rename of private staging). An interrupted `sprite
cancel` names the run and says its request may or may not have been delivered;
check `goodflows sprite status <identity>`. `sprite destroy` is described
under [Retention and destruction](#retention-and-destruction).

Time bounds. Each Sprite CLI call is bounded, and a call that exceeds its
bound is killed and reported as a transport failure (exit 3; a create that
times out is treated like a lost acknowledgement and is never initialized):

| Call | Bound |
|---|---|
| Listing, create, destroy, `sprite org list` and short agent operations (status, ownership checks, supervisor readiness, cancel) | 2 minutes |
| Uploads (runner install, source submission, which the Sprite itself limits to 10 minutes plus handoff) and `sprite logs` | 15 minutes |
| Result download (`collect`, and collection after an attached run) | 2 minutes without any data |
| Runner release download (per request) | 5 minutes |

The attached follow stream has no overall limit, so long runs are never cut
off: a connection that delivers nothing for 2 minutes is dropped and
reconnected without replaying what was printed. It counts as working if it
had delivered something; a session that stalls from its first byte does not,
so stalled sessions end with exit 3 once the 5-minute budget is used up.
`run` reports a failure of its first `sprite org
list` as a preflight error (exit 1).

## Output, exit codes and results

`--json` stdout holds only versioned events. Lifecycle events are
`sprite_*` (`sprite_snapshot`, `sprite_run`, `sprite_phase`,
`sprite_hook_output`, `sprite_collected`, `sprite_timing`, `sprite_unresolved`,
...). Hook output is always structured, never bare lines. Native workflow
events are relayed unchanged, and controller collection and publication
events come before the root's final `summary`. Workflow-call jobs run on the
Sprite exactly as locally: each called workflow is a child invocation of the
same remote runner with its own run id, events, `summary` (relayed as it
arrives, before the root's), logs, history and reporting spool under the
run's `results/`, and the same `graph_sha256` a local run reports. Remote `log_dir`/`history_file`
paths in the summary are remote paths; the bundle maps them locally.

Timings: `sprite_timing` reports snapshot, provisioning, transfer and
collection time on the controller plus the remote phase timeline. They are
never added to the native `duration_ms`/`execute_ms`, and run history host
details describe the Sprite.

| Exit | Meaning |
|---|---|
| `0` / `1` / `130` / `143` | Known workflow result with required collection complete (same as local) |
| `1` | Config, preflight, conflict, ownership refusal, organization mismatch (the platform reports another or no organization for the credential, in `run` and every `sprite` command; exit 3 instead when `sprite destroy` sees it after its delete call), incompatible or unpublished runner, or preparation failure (no fabricated job results) |
| `130` / `143` | Also: Ctrl-C (SIGINT) or SIGTERM before submission (nothing was submitted), or an interrupted `sprite` command; an interrupted `sprite destroy` may have issued its delete call, and an interrupted `sprite cancel` may have delivered its request (see [Cancel versus disconnect](#cancel-versus-disconnect) and [Retention and destruction](#retention-and-destruction)) |
| `2` | Invalid command-line arguments |
| `3` | Unresolved remote execution, transport failure (including an unreachable platform in `sprite status/logs/collect/cancel`), a lost cancellation acknowledgement, an ownership check in `sprite destroy` that failed or went unanswered without a definite refusal (nothing is deleted), or incomplete required collection; the output names the run for recovery |

Text-mode rerun hints keep the original identity, profile and config spelling:
`goodflows run ci --config=custom.toml --env=test --sprite=T-1 --jobs=test`.

### Result bundles

Before the first submission the controller writes a private receipt to
`.goodflows/sprites/<scope-digest>/<run-uuid>/receipt.json` in the directory
you ran from (git ignores it through the `.goodflows/.gitignore` goodflows
writes). An attached run then
collects the final bundle into `.../<run-uuid>/result/`; `sprite collect
--output DIR` uses `DIR` as the base instead. A collection of an unfinished run
goes to a new `partial-<ms>/` snapshot and never replaces another partial or
the final bundle. Repeating an identical final collection succeeds; different
content is a conflict and nothing is overwritten.

| Bundle path | Remote source |
|---|---|
| `bundle.json` | the collection manifest: `remote_root` (`~/.goodflows/runs/<uuid>`, as an absolute path) and, per file, its bundle-relative `path`, `remote_path`, `size` and `sha256` |
| `record.json` | `~/.goodflows/runs/<uuid>/record.json` (phases, timings, terminal state, controller provenance) |
| `header.json` | submission metadata (config text, manifest, `pass_env` names; no values) |
| `events.jsonl`, `workflow.jsonl`, `workflow.stderr`, `worker.stderr` | lifecycle events, native workflow events, runner stderr |
| `hooks/<setup|resume>.<stdout|stderr>` | hook output |
| `results/logs/...` | raw job logs when the run used `--log-dir` |
| `results/history/<workflow>/...` | run history when enabled |
| `results/reports/<uuid>/...` | reporting spool, one per invocation (called workflows use their own run id) |

On a Sprite run `--log-dir`, `--history-dir` and configured history or spool
directories select an artifact kind; the files go to the managed
`results/` directory of that run, not to the controller path. Local path
semantics are unchanged. Only these known result classes are collected —
never arbitrary workdir output. Collection verifies names, entry types,
containment, sizes and hashes against the inventory in private staging, with
finite limits of 10,000 files and 512 MiB per bundle. The Sprite checks those
limits from file sizes before it reads any file, keeps at most 64 MiB of small
files in memory and streams larger files (hashing them on the way, so a large file
rewritten during the export fails that collection; collect again). The
controller stops reading an export once it exceeds 576 MiB (the limit plus
archive overhead), before checking anything. To find a summary path in the bundle, strip
`remote_root` from it (for example `log_dir`
`/home/sprite/.goodflows/runs/<uuid>/results/logs/ci/<run>` is
`results/logs/ci/<run>` in the bundle) or look up `remote_path` in
`bundle.json`. A collection killed midway can leave a private `.collect-*`
staging directory; it is never published, and the next collection into the
same directory removes it.

The remote source has no `.git`. While it builds the archive, the controller
records the snapshot's provenance: the `HEAD` commit, the branch when `git
symbolic-ref` reports one, and `dirty`. It sends them in the submission header
and keeps them in the control record. The remote runner writes that `source`
into the root's and every child's history file, test-report records and run
record. Jobs and hooks never see these values; `pass_env` and argv do not
carry them. `dirty` describes the archive, not `git status`: it is true when a
tracked file's shipped bytes or executable bit differ from `HEAD`, when a
tracked file is missing (deleted, or excluded from the snapshot), or when a
nonignored untracked file is shipped. Ignored files and `.git` do not count.
The shipped bytes are hashed without Git filters, so files whose checkout
differs from their blobs make `dirty` true even in a clean checkout: Git LFS
files, files with another clean filter, and files with line-ending conversion
such as `eol=crlf`. A detached `HEAD` has no `branch`; GitHub Actions
variables are not used in its place. When `HEAD` cannot be read there is no
`source`.

### Reporting

Remote jobs never receive the publisher key: the remote run collects and
finalizes its spool with publication unavailable. After collecting, an
attached controller publishes best-effort with its own credentials, only when
the spool matches this invocation's run, project, workflow, profile,
jobs/partitions and configured credential variable names. A called workflow's
spool is published too, checked the same way against its own run id and
target workflow, and only when its id is a canonical UUID and a summary of
this run (the root's, or an accepted caller's) lists it as a call's
`child_run_id`. Publication happens after collection: child spools first, in
the order their summaries arrived, then the root's, and the root report comes
last. Unlike a local run, a child's upload lines therefore follow the child's
own report (and any later output) rather than preceding it. Child spools are
published only when the root summary was received (it lists the root's calls);
otherwise replay one explicitly with `goodflows reports upload`. A mismatch is
refused without looking up any other variable. Publication never changes the
workflow's exit code. `sprite collect` never publishes; replay a collected
spool explicitly with `goodflows reports upload <bundle>/results/reports/<uuid>`
only for invocation data you trust.

With `[reporting] history = true`, each remote invocation, the root and every
called workflow, freezes its run record (`run-record.json`) in its own spool,
without a key, and sends nothing. Its relayed `run_upload_pending` event says
`credentials_unavailable` because the record waits for the controller. After
collection the controller publishes the run records with the reports, each
after its spool's reports: child spools first, then the root's. The
`reports_upload_summary` that follows each spool gives the record's final
state. A spool that lists a run record is refused, and nothing from it is
sent, unless the controller's own configuration sets `[reporting] history =
true`. A remote config that turns the upload on therefore cannot make the
controller send command text. A detached run's record waits in the collected
bundle for `reports upload`. Delivery never changes the exit code.

A remote record's `source` is the snapshot provenance described above:
`commit`, the archive-derived `dirty`, and `branch` only when git reported a
branch. Its `host` is the Sprite's, and its `duration_ms` is the remote
invocation's. The remote runner's environment is the minimal base plus
forwarded values, so the record has `ci` only when `[sprite] pass_env` lists
`GITHUB_ACTIONS` and the other GitHub variables the README names:

```toml
[sprite]
pass_env = ["GITHUB_ACTIONS", "GITHUB_EVENT_NAME", "GITHUB_RUN_ID",
  "GITHUB_RUN_ATTEMPT", "GITHUB_REF", "RUNNER_NAME", "GITHUB_SERVER_URL",
  "GITHUB_REPOSITORY"]
```

Forwarding has the usual effects. The values also reach hooks and jobs. An
unset name fails the run before anything remote happens, so this config runs
remotely only where every name is set. A forwarded value of 8 bytes or more
also excludes every file that contains it from the snapshot (see
[What is sent](#what-is-sent)), and an excluded tracked file makes `dirty`
true. Values such as `GITHUB_SERVER_URL=https://github.com` and
`GITHUB_REPOSITORY` often appear in tracked files. Forwarding these variables
never supplies `source`.

## Retention and destruction

Sprites, caches and run directories are kept after success, failure,
cancellation and disconnect, and GoodFlows never prunes remote runs
automatically: retained storage keeps costing money. Remote storage only
grows. Each run keeps `~/.goodflows/runs/<uuid>/` on the Sprite: a full copy
of the snapshot (`source/`, about the uncompressed size of the `sprite_snapshot`
files) plus its events, hook output and `results/`. Each distinct GoodFlows
build used with the identity keeps its runner under
`~/.goodflows/runners/`, and your hooks' state directory grows as they decide.
Nothing of this is removed after a run, because recovery and collection read
the run directory and the plan deliberately leaves automatic remote retention
out. The cleanup path is to collect what you need and destroy the identity's
Sprite; the next run creates and prepares a fresh one. Deleting files inside
the Sprite yourself (for example old `runs/<uuid>/source/` directories of
finished runs) is possible, but GoodFlows does not do it or manage it.
Remove an identity explicitly:

```bash
goodflows sprite destroy T-1          # asks for confirmation on a terminal
goodflows sprite destroy T-1 --yes    # automation
```

Destruction verifies ownership, refuses while a run is active or unresolved
(cancel or reconcile first), blocks new runs during the operation, checks the
resource id again immediately before deleting, and never deletes a
same-named replacement after an ambiguous acknowledgement. Dependency state
and uncollected results are lost permanently. Sprite and publisher
credentials stay on the controller throughout.

While a destroy is between its ownership check and confirmed deletion,
`sprite status` shows `destroying: true` ("destruction in progress") and new
runs are refused. Each destroy holds its own claim on that marker, so
concurrent destroys never clear each other's. After the delete call the
destroy lists the Sprite again every 2 s for up to 60 s, because the platform
may finish a delete after accepting it:

- The Sprite disappears: destroyed (exit 0).
- The platform definitely refused the delete call (an `API error (status
  4xx)` other than 408) and the same Sprite is still listed after 60 s:
  nothing was deleted, the destroy drops its claim and the identity accepts
  runs again (exit 3).
- The delete call succeeded, or failed without a definite refusal (a
  transport error, a timeout or a server error, which may hide an accepted
  delete), and the Sprite is still listed or cannot be listed: the deletion
  may still be in progress, so the claim stays and runs stay refused (exit 3).
  Check `goodflows sprite status <identity>`; once the Sprite is gone it
  reports `absent`. If it is still there, run `goodflows sprite destroy
  <identity> --yes` again.
- A failure before the delete call (the re-check listing fails, the
  resource changed, or this destroy's claim is no longer there) drops the
  claim (exit 1 or 3) and deletes nothing.

Two cases keep a claim on purpose:

- The destroying controller was killed or hung in that window. Its claim
  stays until a later `goodflows sprite destroy <identity> --yes` finds it
  older than 15 minutes and removes it. That later destroy also deletes the
  Sprite if it can. A destroy gives up after 10 minutes of its own (monotonic)
  time, but a controller host that is suspended does not count that time
  while the claim's age on the Sprite keeps growing. So each destroy checks,
  just before its delete call, that its claim still exists, and deletes
  nothing if it was swept. A small window remains between that check and the
  delete call. Also, once a stale claim of a destroy whose delete was accepted
  has been swept, a later destroy that the platform refuses readmits runs
  even if that earlier delete is still finishing.
- The platform reported another or no organization during the destroy
  (organization mismatch). GoodFlows then runs nothing more in that Sprite,
  not even the cleanup. If that happens before the delete call, `sprite
  destroy` exits 1, like any organization mismatch; if it happens while
  re-listing after the delete call it exits 3, because the delete may have
  happened. Fix the credential (use one for the requested organization, or
  set `SPRITE_ORG` to the credential's organization), then run `goodflows
  sprite destroy <identity> --yes` again.

An abort that dies between removing its claim and the marker directory
leaves an empty marker; `goodflows sprite status <identity>` removes it.

If the ownership check fails in any way other than a definite refusal (an
unowned or differently scoped Sprite, or one with no GoodFlows runner: exit 1)
or an organization mismatch (exit 1), for example a timeout, a transport error
or a lost reply, the destroy deletes nothing and exits 3, because the check may have reached the Sprite and written a
claim whose token only the Sprite knows. Such a claim keeps refusing new runs
(`sprite status` shows `destroying: true`) until a `goodflows sprite destroy
<identity> --yes` run 15 minutes or more later sweeps it; that destroy also
deletes the Sprite if it can. Running destroy again sooner deletes the Sprite
as well, but leaves the old claim until it is stale. There is no way to clear
such a claim without destroying: `sprite status` deliberately does not sweep
old claims, because a claim older than 15 minutes can still belong to a
delete in progress (an accepted delete whose claim is kept on purpose, or a
suspended controller between its claim check and its delete call), and
readmitting runs then could start work on a Sprite that is about to
disappear.

Ctrl-C (or SIGTERM) during `sprite destroy` exits 130 (143):

- At the confirmation prompt, or before the ownership check: nothing was
  changed.
- The ownership check itself is never interrupted midway; if it does not
  finish within its 2-minute bound the destroy exits 3 (see below).
- After the ownership check and before the delete call: the destroy removes
  its own claim, so nothing was deleted and runs are admitted again. If that
  removal fails, the output says so and the claim stays.
- Once the delete call has been issued: the claim stays, because the delete
  may be in progress, and new runs stay refused. Check `goodflows sprite
  status <identity>` and run `goodflows sprite destroy <identity> --yes`
  again.

## Dogfooding GoodFlows

`examples/sprites/goodflows.toml` runs this repository's own checks on a
Sprite with `scripts/sprite-setup.sh` (installs rustup and Rust 1.88.0 into
`$GOODFLOWS_SPRITE_STATE_DIR`, detecting a rustup wrapper that is not a real
installation) and `scripts/sprite-resume.sh` (verifies the toolchain and
reuses the persistent Cargo cache without rebuilding the environment):

```bash
goodflows run --config examples/sprites/goodflows.toml --sprite "$THREAD_ID" --json
```

The same file also runs locally (`goodflows run --config
examples/sprites/goodflows.toml`): without `GOODFLOWS_SPRITE_STATE_DIR` the
jobs use the installed toolchain. Remotely, the Cargo registry and
dependency builds in the state directory are reused across runs, but each run
has a new source path, so Cargo rebuilds the workspace crate itself every
time; expect subsequent remote runs to be slower than a warm local tree.
