# Running workflows on Sprites

`goodflows run --sprite <identity>` runs a complete workflow on a
[Sprite](https://sprites.dev) that belongs to one caller identity of one
project. GoodFlows creates or reuses that Sprite, delivers a verified snapshot
of your working tree, runs your explicit preparation hook, executes the
unchanged workflow with the same GoodFlows version, and brings the results
back. Without `--sprite`, nothing changes. Runs stay local and never call the
Sprite CLI.

> **Support status.** Contract tests against a fake Sprite CLI cover local
> behavior, configuration, preflight, snapshotting, ownership, locking,
> recovery, collection and destruction. Live acceptance on disposable Sprites
> in a platform-verified organization covered controller-kill survival, worker,
> supervisor and service loss, the fault matrix and the dogfood comparison.
> **Recovery after an actual cold boot or cold wake (a changed boot identity)
> is unverified and unsupported.** Live acceptance did not exercise it, and it
> is pending future validation. Unchanged-boot service restarts, suspend/resume
> and checkpoint/restore are not cold boot and do not count as evidence of it.
> Treat VM loss as `interrupted`, never as resumable.

## Requirements

- The official Sprite CLI must be on `PATH` and authenticated (`sprite login`,
  or `sprite auth setup --token ...` in automation). GoodFlows is verified
  against `sprite version 2026-11-03 (4eb1509)`.
- Git. GoodFlows takes the snapshot from the Git working tree that contains the
  config.
- A Linux runner for the Sprite's architecture. A Linux controller of the same
  architecture installs itself. Other controllers (for example macOS) download
  the exact version's checksummed Linux release
  (`goodflows-v<version>-<arch>-unknown-linux-gnu.tar.gz`).

  A Linux controller's own executable can install but fail to run on the
  Sprite, for example because it needs a newer glibc than the Sprite image has.
  If the Sprite still runs a plain command in that case, GoodFlows emits a
  `sprite_runner_fallback` event and installs the exact version's release
  instead. The event gives the reason, the release runner id, and whether the
  release came from the cache. Once GoodFlows verifies the release's published
  checksum, it caches the release and its digest on the controller in
  `.goodflows/sprites/runners/v<version>-<arch>/`. That directory sits under the
  Git root of the selected config, so each project has one cache, whichever
  directory you run from.

  The digest only detects a damaged copy. GoodFlows stores it next to the
  binary, so it proves nothing about who wrote the binary. GoodFlows checks a
  cached copy's integrity against the digest. It uses the copy only while the
  directory is private to you (owned by your user, mode 0700), both files are
  yours and not writable by others, and Git tracks nothing there. Otherwise
  GoodFlows downloads the release and verifies its checksum again. It does not
  rewrite a non-private cache directory.

  Before falling back, GoodFlows asks an already installed copy of its own
  build a second time, so one failed call never switches runners. GoodFlows
  never uploads a runner again when the Sprite already has one with the same
  content, so later runs from such a controller transfer nothing extra.

  The fallback runs the *published* build of the same version. GoodFlows has no
  marker that tells a released executable from a local build of unreleased
  changes with the same version. A development build that falls back therefore
  runs the release's code on the Sprite, not its own, and the event says so.
  When you test unreleased changes, use a build that runs on the Sprite (or an
  official release). An unreleased version without a matching Linux artifact
  fails before any hook or job. GoodFlows never uses `latest` or a `goodflows`
  already on the Sprite.

You do not need a Spry binary or library.

## Sprite CLI contract

This contract comes from `sprite --help` and `<command> --help` of the official
CLI `sprite version 2026-11-03 (4eb1509)`, and from the
[Sprites API](https://docs.sprites.dev/api/dev-latest/sprites/) and
[Tasks API](https://docs.sprites.dev/keeping-sprites-running) documentation.
The CLI came from the installer `https://sprites.dev/install.sh`, and its
`linux-x86_64` binary has SHA-256
`731af477ecf7328fd188c67b3649874c9107b6f0097c25888d0dd17aad823527`, which
matches the release manifest. `sprite version` is not a command. Use
`sprite --version`.

| Concern | What GoodFlows relies on | Evidence |
|---|---|---|
| Organization selection | Every call passes `-o <org>`. GoodFlows resolves the org once from `SPRITE_ORG`, else from the `(current)` entry of `sprite org list`. `-o` is **not** proof of where the CLI acts. Before any receipt, create, exec or destroy, and before reading status, logs or results, GoodFlows requires the listing's top-level `org.name` and every listed Sprite's `organization` to equal the requested org. Otherwise it refuses with exit 1 and names both orgs | The global flag is `-o, --org`. The docs describe `SPRITE_ORG` as an override, and `org list` marks the default. The CLI reference describes `SPRITE_TOKEN` as an "API token override", and tokens have the form `org-slug/org-id/token-id/token-value`. The API lists Sprites "for the authenticated organization". In a live observation (2026-11-07), `sprite list -o goodway --json` under a `SPRITE_TOKEN` for another org reported `"org":{"name":"thomas-brewer",…}` and `"organization":"thomas-brewer"` on every Sprite, with no warning. Static inspection of the installed binary finds a `SPRITE_TOKEN` override but no `SPRITES_TOKEN` string, so `SPRITES_TOKEN` alone does not appear to select the CLI's credential |
| Sprite selection | `exec -s <name>`. `create`, `destroy` and `info` take the name positionally. GoodFlows never calls `sprite use`, which writes a `.sprite` file | `exec --help`, `use --help` |
| Creation | `sprite create -o <org> --skip-console <name>`. Only a zero exit of this call counts as "we created it" | `create --help`. API `POST /v1/sprites` returns 201, and **400 for an invalid body or an existing name**. A conflict is therefore indistinguishable from other bad requests, and GoodFlows never treats it as success |
| Stable identifier | `sprite list -o <org> --prefix <name> --json`, using the `id` field when present | API create and get responses carry `id`. The docs describe the list response inconsistently (`name`, `org_slug`, `updated_at` in one version). Destroy fails closed. It refuses unless the listed `id`, the id recorded at creation and the id seen at lookup are all present and equal, and it refuses any listing with more than one Sprite of the name. A live check (2026-11-07, org list of real Sprites) returned `{"org":…,"sprites":[{"created_at","id","name","organization","status","updated_at","url","url_settings"}]}`, with `id` of the form `sprite-<uuid>`. GoodFlows still refuses a listing without an id |
| Streaming stdin | `exec --stdin` (WebSocket, non-TTY) for the archive, and `--no-stdin` for everything else | `exec --help` lists `--stdin`, `--no-stdin` and `--http-post` (HTTP/1.1, 10 MiB stdin cap per API docs) |
| Port forwarding | `--no-port-forward` on every exec | `exec --help` |
| Session lifetime | Not relied on. Non-TTY sessions suspend on detach. In the lifecycle probe, a plain exec stopped about 10 s after the probe killed its controller | `sessions --help`; probe evidence |
| Durable work | `sprite-env services create <name> --cmd ... --args a,b --dir ... --no-stream` inside the Sprite. The Tasks API `PUT /v1/tasks/<name>` (upsert, max 1 h, returns 200) and `DELETE` (204/404) over `/.sprite/api.sock`, host `sprite` | Tasks API docs; probe evidence (sprite-env 0.0.4) |
| Destruction | `sprite destroy -o <org> --force <name>` after GoodFlows' own confirmation | `destroy --help` (`--force` skips its prompt) |
| Names | `<prefix>-<32 hex>`, 3 to 63 lowercase letters, digits and hyphens, alphanumeric at both ends | Sprites staff answer on the Fly.io community forum (no formal reference found) |
| Credentials | `SPRITE_TOKEN` and `SPRITES_TOKEN` stay in the CLI's environment or keyring, never in argv | `auth setup --help` takes `--token` in argv, so GoodFlows never calls it |

## Configuration

```toml
[settings]
sprite_prefix = "goodflows"   # project-wide; required for every Sprite operation

[sprite]
setup = "bash scripts/sprite-setup.sh"    # runs until it succeeds once
resume = "bash scripts/sprite-resume.sh"  # runs on every later prepared run
pass_env = ["DATABASE_URL"]               # caller variables forwarded (default: none)
```

- `sprite_prefix` takes 1 to 20 lowercase ASCII letters, digits or hyphens, and
  starts with a letter or digit. It is valid only in the top-level
  `[settings]`. A workflow or profile that sets it fails validation.
- `[sprite]` is top-level only. `setup` and `resume` must be nonempty shell
  commands when you run remotely. Write `true` for a deliberate no-op.
  GoodFlows never discovers scripts (it ignores an `.agents/setup` file) and
  never infers toolchains or dependencies.
- `pass_env` lists unique variable names. Unset names fail before anything
  remote happens. An empty value counts as set.
- Partial configurations stay valid for `validate` and `list`, which are
  offline. They need no Sprite CLI, credentials or allowlisted values, and they
  show names, never values.

`--env` keeps its meaning. It selects a workflow profile, never a location.

## Command line

```text
goodflows run [workflow] --sprite <identity> [existing run flags]
goodflows sprite status  <identity> [--json]
goodflows sprite logs    <identity> --run <uuid> [--json]
goodflows sprite collect <identity> --run <uuid> [--output <directory>] [--json]
goodflows sprite cancel  <identity> --run <uuid> [--json]
goodflows sprite destroy <identity> [--yes]
```

The identity is opaque, case-sensitive caller data (1 to 256 UTF-8 bytes, no
control characters), such as an agent thread id. GoodFlows never uses it as a
Sprite name or shell text. The `sprite` subcommands accept `--config` and
`--json`. They reject the run-only flags (`--env`, `--jobs`, `--parallelism`,
`--log-dir`, history flags) instead of ignoring them. They need only the
project prefix, not the current source, hooks or run-only variables.

### Names, ownership and sharing

Within the organization, the prefix and identity select one Sprite named
`<prefix>-<32 hex>`. The hex part comes from a SHA-256 over a versioned,
length-delimited tuple, so case or punctuation differences never merge
identities. The name is at most 53 bytes, within the platform's 3 to 63
lowercase/digit/hyphen rule. The Sprite stores a private ownership record
(`~/.goodflows/owner.json`) with the organization, prefix, exact identity and
digests. Every operation verifies it.

- Projects that must not share environments must use different prefixes.
  Projects with the same prefix in the same organization share identities.
- GoodFlows never adopts, prepares, overwrites or destroys a Sprite that has
  the computed name but no matching record. A crash between creation and
  initialization, or a lost create acknowledgement, leaves such an unowned
  Sprite for manual reconciliation (`sprite destroy -o <org> <name>` after you
  have checked it). The same happens when `sprite list` does not report a new
  Sprite with its resource id within 10 listings a second apart. The ownership
  record must hold that id, because destruction verifies it, so GoodFlows
  refuses the run (exit 1) without initializing the Sprite.
- Ownership is a consistency check, not an authorization boundary. Other
  organization administrators and jobs running as the same user can change it.

The organization is `SPRITE_ORG` when set, otherwise the Sprite CLI's current
default organization (`sprite org list`). Every call names the organization
and Sprite explicitly. GoodFlows never runs `sprite use` or changes the active
context, never exposes URLs, and disables port forwarding.

Naming the organization does not choose it. The Sprite CLI acts in the
organization its credential belongs to (`SPRITE_TOKEN` when set, otherwise the
stored token). A listing under `-o <other>` can silently describe the
credential's own organization instead. Every remote command therefore lists
the identity's Sprite first. It verifies that the platform-reported
organization (`org.name`, and `organization` on each listed Sprite) is present
and equal to the requested one. On a mismatch or a missing field, it stops
with exit 1 before any receipt, create, exec or destroy, and before reading
another organization's status, logs or results. The diagnostic names both
organizations, never a credential value. To fix it, use a credential for the
intended organization, or set `SPRITE_ORG` to the credential's organization
when that is the intended target.

### One run per identity

A remote, process-held lock per organization, prefix and identity admits one
run at a time across controllers, workflows and profiles. The lock is separate
from run-UUID idempotency. It covers upload, preparation, execution,
descendant cleanup and durable finalization. The submitting process hands the
locked descriptor to the supervisor, which passes it to the run's worker at
exec, so the lock is never free in between. Hooks and jobs do not inherit it.
The durable active-run record and process-liveness checks still apply as a
second line of defense.

A contender fails at once with the active run's UUID and creates no run
record. This holds even when the contender's controller uses a different
GoodFlows build. Before it installs a different runner, such a contender asks
the installed runner whether a run is live, and stops without installing
anything. That check is advisory. On first use, or when the installed runner
cannot answer, the contender installs the new build first, and admission
reports the conflict:

```text
Error: Sprite identity T-1 is busy with run 6d1c...; it was not modified (check `goodflows sprite status`)
```

Disconnecting does not free the identity. Only a durably finalized run frees
it, or a run reconciled as `interrupted` after every recorded process is
proven gone. Submitting a recorded UUID again only looks it up and never
reruns it. Different identities never serialize each other.

## What is sent

The snapshot is the current working tree of the Git repository that contains
the config. It holds tracked files with their current contents (not `HEAD`)
and nonignored untracked files, from
`git ls-files --cached --others --exclude-standard`. Deleted files are absent.
Your own ignore rules leave out ignored files (build output, caches).
GoodFlows copies files once into a private, read-only staging directory and
computes the manifest (paths, sizes, SHA-256, executable bit) from those
copies. A file that changes during the copy fails the run.

GoodFlows always excludes the following, and shows a path and reason but
never contents: `.git`, `.amp`, `.sprite`, `.goodflows`, `.env` and `.env.*`,
`.netrc`, `.npmrc`, `.pypirc`, `.git-credentials`, `credentials*`,
`secrets.*`, private-key files (`id_rsa`, `id_ed25519`, `*.pem`, `*.key`,
`*.p12`, `*.pfx`, `*.jks`, `*.keystore`), anything under `.ssh`, `.aws` or
`.gnupg`, files containing a recognized private key, and files containing the
value of a controller credential (`SPRITE_TOKEN`, `SPRITES_TOKEN`, the
reporting `token_env`) or of a forwarded variable at least 8 bytes long. This
is a conservative heuristic, not a secret scanner. It excludes synthetic test
keys too.

GoodFlows rejects the following before anything remote: special files,
submodules (gitlinks), unresolved, cyclic or external symlinks (it never reads
their targets), file/directory path collisions, a config or path-based hook
script that is not in the snapshot, any declared workdir outside the Git root
(including unselected workflows and profiles), and non-Git directories. It
delivers internal symlinks as copies of the in-root file they name.

Workflows that need `.git` (for example `git describe`) or absolute controller
paths in command text do not work remotely. GoodFlows does not rewrite your
commands.

The archive is one gzip tar that GoodFlows streams over the CLI's WebSocket
stdin (`sprite exec --stdin`), never the 10 MiB-bounded HTTP POST mode.
GoodFlows extracts it into a fresh `runs/<uuid>/source.incoming/`, verifies it
against the manifest (inventory, hashes, sizes, modes), and only then renames
it to `source/`. An interrupted or mismatched transfer leaves no runnable tree
and never touches earlier runs.

## Preparation, state and environment

After source activation, exactly one hook runs (`sh -c`, null stdin) in the
selected workflow's workdir. `setup` runs until it has succeeded once, and
`resume` runs afterwards. GoodFlows records the Sprite as prepared only after
a successful setup. GoodFlows retries a failed setup next time. A failed
resume does not clear preparation. A lost setup result counts as unprepared.
Changing the hook text does not silently re-run setup. Instead, the run
reports a `sprite_setup_changed` event and still runs `resume`. Use `resume`
for migrations, or destroy and recreate an idle Sprite. If either hook fails,
no job starts (exit 1).

Hooks and jobs get `GOODFLOWS_SPRITE_STATE_DIR`, an absolute directory outside
the per-run source that persists for the identity. Your scripts own caches
and toolchains there and link them into each fresh source tree. GoodFlows
never discovers, mounts or copies caches. GoodFlows kills every process a hook
starts when the hook ends, so a hook cannot leave a server running for the
jobs. Start long-lived services with `sprite-env services` (they outlive the
run, and you own their lifecycle) or inside the job that needs them. The
variable is reserved. Configured env and `pass_env` cannot set it, and local
runs remove any inherited value.

A hook's `export` does not reach later commands, because there is no shared
shell session. Use configured env, fixed executable paths or scripts.
GoodFlows does not assume that login-shell `PATH` changes persist.

Hooks and jobs start from a minimal base environment (`HOME`, `USER`,
`LOGNAME`, `PATH`, locale, `TZ`) plus forwarded values and runner-owned
variables. GoodFlows forwards nothing else from the controller. For jobs, the
forwarded values are the lowest-precedence caller layer, below settings env.
Above settings, precedence rises through workflow, profile, job and step env.
Hooks receive the exact forwarded values.

Remote runs deny the following names in `pass_env` and in every configured env
layer: `SPRITE_TOKEN`, `SPRITES_TOKEN`, every reporting `token_env`, and the
runner-owned `GOODFLOWS_RUN_ID`, `GOODFLOWS_REPORT_DIR`,
`GOODFLOWS_PARTITION`, `GOODFLOWS_PARTITION_COUNT`, `GOODFLOWS_ENVIRONMENT`,
`GOODFLOWS_OUTPUT` and `GOODFLOWS_SPRITE_STATE_DIR`. You can forward a
reporting `url_env` explicitly. Forwarded values travel as a separate private
input frame to the supervisor and worker processes, never in argv, archives,
metadata or diagnostics. GoodFlows never writes them to disk. Hooks you give a
secret can still print or store it, and GoodFlows does not redact raw output.

Trusted setup and resume scripts may use privileged installation where the
Sprite allows it. The workflow runner starts with the ambient, bounding,
effective, permitted and inheritable capability sets dropped. When the
bounding set cannot be dropped, `no_new_privs` takes its place. GoodFlows
verifies this before launch. If it cannot establish that state, the run ends
as `preparation_failed` (exit 1, no job started) rather than skipping
permission-sensitive tests. A reused Sprite with its caches and services is
one trust domain. Fresh source directories do not isolate hostile jobs.

## Execution and recovery

The controller hands the accepted run to a Sprite-managed service,
`goodflows-supervisor`, which it creates with `sprite-env services create`.
The service starts a worker per run. The worker registers its boot id, PID,
start time and execution generation before the supervisor lets it start. It
holds the caller lock it inherited from the submission. It renews a Tasks API
wake hold (`PUT /v1/tasks/gf-<uuid>` with a 12 s expiry about every 3 s). It
runs the hook and the workflow runner, kills remaining descendants, flushes
results and writes the terminal record. Only then does it free the identity
and delete the hold. A plain `sprite exec` stops about 10 s after its
controller dies, so it never owns execution. GoodFlows checks service creation
by protocol readiness and the expected new generation, not by the CLI's exit
status.

If the worker itself hits an error after starting a hook or the workflow
runner, it kills every process left in its session before it records
`interrupted` and frees the identity. If anything survives that, the worker
exits but leaves the active pointer and the nonterminal record in place.
Reconciliation then keeps the identity busy until those processes are gone.

An identity can stay busy for good. This happens when a recorded process never
dies, or when GoodFlows cannot read a Sprite's boot id. `goodflows sprite
destroy` refuses in that case too, because it never deletes under an active or
unresolved run. The last resort is the Sprite CLI itself, after you have
checked the Sprite. Run `sprite destroy -o <org> --force <name>` (the name is
in `sprite status`). It deletes the environment and every uncollected result.

If renewal fails, GoodFlows blocks admission at once and stops the run before
the hold can expire. The identity stays held until the run is finalized. If
the worker dies before it finalizes, reconciliation records the run as
`interrupted` (in `sprite status`, admission or supervisor startup) and also
removes that run's admission block. Renewal stops as soon as the terminal
record is durable, and the worker still deletes the hold itself last.
Reconciliation also removes a block left by a run whose record is already
terminal. GoodFlows never treats hold expiry as proof that work stopped.

A run moves through the phases `accepted`, `uploading`, `verifying`, `setup`
or `resume`, `running` and `finalizing`. It then ends in one of `completed`
(with the workflow's own exit code), `preparation_failed`, `transfer_failed`,
`cancelled` or `interrupted`. `unreachable` is only a controller observation.

You can recover from any controller with the same config prefix:

```bash
goodflows sprite status T-1                    # ownership, preparation, active and recent runs
goodflows sprite logs T-1 --run <uuid>         # saved lifecycle and workflow events
goodflows sprite collect T-1 --run <uuid>      # verified local bundle; never publishes
goodflows sprite cancel T-1 --run <uuid>       # explicit cancellation of that run only
```

`status` may record `interrupted` after reconciling under the caller lock. It
does so only when the worker, the workflow runner and every recorded session
are proven gone (boot id, PID start time and generation checks). A surviving
descendant keeps the identity blocked. `logs` and `collect` only read. None of
these commands transfers source, runs hooks or reruns jobs. When the
supervisor service starts, it reconciles the same way before it accepts work,
and it never relaunches recorded runs.

An acknowledged `cancel` stops a running hook or workflow. If the cancellation is
acknowledged before the hook or the workflow starts (during upload,
verification or handoff, for example), the run ends `cancelled` without
starting. A Sprite whose setup never ran stays unprepared, so the next run
runs setup.

Here is an example `sprite status --json` event with nonsecret fields only:

```json
{"type":"sprite_status","identity":"T-1","sprite":"goodflows-0f3c...","state":"active",
 "message":"Sprite goodflows-0f3c... (identity T-1)\n  prepared: yes\n  active run: 6d1c...",
 "busy":true,"active_run_id":"6d1c...","prepared":{"run_id":"2a9e...","setup_digest":"...","prepared_at_ms":1762500000000},
 "degraded":false,"destroying":false,"reconciled":true,
 "runs":[{"run_id":"6d1c...","phase":"running","workflow":"ci","environment":null,"accepted_at_ms":1762500100000,"terminal":null,"timings":[...],"path":"/home/sprite/.goodflows/runs/6d1c..."}],
 "owner":{"organization":"my-org","prefix":"goodflows","identity":"T-1","name":"goodflows-0f3c...","sprite_id":"..."},
 "supervisor":{"protocol":1,"generation":"...","runner":"...","version":"0.7.0"},"schema_version":1}
```

Reconciliation has limits. GoodFlows reports platform or VM loss as
`interrupted` with partial results and never restarts the run automatically.
Every process of a run stays in the worker's session, and hooks and the
workflow runner get their own process groups inside it. An unregistered
descendant therefore still blocks the identity. GoodFlows cannot track a
descendant that leaves the session (`setsid`, daemonizing).

Measurements on a Sprite showed two cases. Killing only the supervisor process
leaves the worker running, and the run completes. Killing the service
(`sprite-env services signal ... KILL`) also kills its workers, and the
restarted supervisor records `interrupted` at startup. All of these kept the
same boot id. If GoodFlows cannot read the Sprite's boot id, it treats no
recorded process as gone, and the identity stays busy.

Recovery after an actual cold boot or cold wake (a VM restart with a new boot
id) is unverified and unsupported, pending future validation. It was not
exercised. Unchanged-boot service restarts, suspend/resume and
checkpoint/restore are not cold boot and are not evidence of it. That includes
a Sprite that `sprite list` reports as "cold" but that resumes with the same
boot id. GoodFlows still fails closed. It treats a recorded worker from an
earlier boot as gone, so reconciliation records `interrupted` and never
replays the run.

### Cancel versus disconnect

Ctrl-C (SIGINT/SIGTERM) on an attached run sends a UUID-bound cancellation to
the supervisor. The supervisor stops the hook's process group or signals the
remote GoodFlows runner, so the existing cancellation logic finalizes the
jobs. A closed terminal, a broken pipe or a lost connection does **not**
cancel, and the run continues. An attached controller reconnects with backoff
(1 s, doubling to 15 s). A reconnect asks the runner to leave out the lines
already printed (`follow --skip <lines>`, acknowledged by the runner). A
runner from before that option ignores it and replays the whole saved stream.
The controller then skips those lines one by one, so nothing is printed twice
either way.

A connection counts as working only if it delivers new output, or if it
delivers something (the acknowledgement or replayed lines) and stays up for
30 s. A connection that never delivers a byte does not count, however long it
stays open. Losing a working connection restarts the 5-minute budget, so a
long quiet job whose stream drops still gets a full budget. Connections that
only replay and drop, or that stall without delivering anything, do not
restart it. After 5 minutes without a working connection, the controller exits
3 with the run reference. If the supervisor does not acknowledge a
cancellation and the controller retrieves no terminal result, the exit code is
3. The identity then stays held until the run finalizes or is reconciled.
Cancelling a finished run signals nothing.

Ctrl-C before the run is submitted (snapshot, provisioning, runner install)
stops at the next safe point, after the Sprite CLI call in progress. It
submits nothing and exits 130 (143 for SIGTERM). It never interrupts a create
or an install halfway. `sprite status`, `logs`, `collect` and `cancel` stop at
once with the same exit codes. They only read or make an idempotent request. A
collection stopped midway leaves either nothing or a complete verified bundle,
because publication is a single rename of private staging. An interrupted
`sprite cancel` names the run and says its request may or may not have been
delivered. Check `goodflows sprite status <identity>`.
[Retention and destruction](#retention-and-destruction) describes
`sprite destroy`.

Each Sprite CLI call has a time bound. GoodFlows kills a call that exceeds its
bound and reports a transport failure (exit 3). A create that times out counts
as a lost acknowledgement, and GoodFlows never initializes that Sprite. The
bounds are:

| Call | Bound |
|---|---|
| Listing, create, destroy, `sprite org list` and short agent operations (status, ownership checks, supervisor readiness, cancel) | 2 minutes |
| Uploads (runner install, source submission, which the Sprite itself limits to 10 minutes plus handoff) and `sprite logs` | 15 minutes |
| Result download (`collect`, and collection after an attached run) | 2 minutes without any data |
| Runner release download (per request) | 5 minutes |

The attached follow stream has no overall limit, so GoodFlows never cuts off
long runs. It drops a connection that delivers nothing for 2 minutes and
reconnects without replaying what was printed. That connection counts as
working if it had delivered something. A session that stalls from its first
byte does not count, so stalled sessions end with exit 3 once the 5-minute
budget is used up. `run` reports a failure of its first `sprite org list` as a
preflight error (exit 1).

## Output, exit codes and results

`--json` stdout holds only versioned events. Lifecycle events are `sprite_*`
(`sprite_snapshot`, `sprite_run`, `sprite_phase`, `sprite_hook_output`,
`sprite_collected`, `sprite_timing`, `sprite_unresolved`, ...). Hook output is
always structured, never bare lines. GoodFlows relays native workflow events
unchanged. Controller collection and publication events come before the root's
final `summary`.

Workflow-call jobs run on the Sprite exactly as they do locally. Each called
workflow is a child invocation of the same remote runner. It has its own run
id, events, `summary` (relayed as it arrives, before the root's), logs,
history and reporting spool under the run's `results/`. It reports the same
`graph_sha256` a local run reports. Remote `log_dir` and `history_file` paths
in the summary are remote paths, and the bundle maps them locally.

`sprite_timing` reports snapshot, provisioning, transfer and collection time
on the controller, plus the remote phase timeline. GoodFlows never adds these
timings to the native `duration_ms` or `execute_ms`. Run history host details
describe the Sprite.

| Exit | Meaning |
|---|---|
| `0` / `1` / `130` / `143` | Known workflow result with required collection complete (same as local) |
| `1` | Config, preflight, conflict, ownership refusal, organization mismatch (the platform reports another or no organization for the credential, in `run` and every `sprite` command; exit 3 instead when `sprite destroy` sees it after its delete call), incompatible or unpublished runner, or preparation failure (no fabricated job results) |
| `130` / `143` | Also: Ctrl-C (SIGINT) or SIGTERM before submission (nothing was submitted), or an interrupted `sprite` command; an interrupted `sprite destroy` may have issued its delete call, and an interrupted `sprite cancel` may have delivered its request (see [Cancel versus disconnect](#cancel-versus-disconnect) and [Retention and destruction](#retention-and-destruction)) |
| `2` | Invalid command-line arguments |
| `3` | Unresolved remote execution, transport failure (including an unreachable platform in `sprite status/logs/collect/cancel`), a lost cancellation acknowledgement, an ownership check in `sprite destroy` that failed or went unanswered without a definite refusal (nothing is deleted), or incomplete required collection; the output names the run for recovery |

Text-mode rerun hints keep the original identity, profile and config spelling,
as in `goodflows run ci --config=custom.toml --env=test --sprite=T-1 --jobs=test`.

### Result bundles

Before the first submission, the controller writes a private receipt to
`.goodflows/sprites/<scope-digest>/<run-uuid>/receipt.json` in the directory
you ran from. Git ignores it through the `.goodflows/.gitignore` that
GoodFlows writes there. An attached run then collects the final bundle into
`.../<run-uuid>/result/`. `sprite collect --output DIR` uses `DIR` as the base
instead. A collection of an unfinished run goes to a new `partial-<ms>/`
snapshot and never replaces another partial or the final bundle. Repeating an
identical final collection succeeds. If the content differs, GoodFlows reports
a conflict and overwrites nothing.

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

On a Sprite run, `--log-dir`, `--history-dir` and configured history or spool
directories select an artifact kind. The files go to the managed `results/`
directory of that run, not to the controller path. Local path semantics are
unchanged. GoodFlows collects only these known result classes, never arbitrary
workdir output.

Collection verifies names, entry types, containment, sizes and hashes against
the inventory in private staging. It enforces finite limits of 10,000 files
and 512 MiB per bundle. The Sprite checks those limits from file sizes before
it reads any file. It keeps at most 64 MiB of small files in memory and
streams larger files, hashing them on the way. A large file rewritten during
the export therefore fails that collection, and you need to collect again. The
controller stops reading an export once it exceeds 576 MiB (the limit plus
archive overhead), before checking anything.

To find a summary path in the bundle, strip `remote_root` from it, or look up
`remote_path` in `bundle.json`. For example, `log_dir`
`/home/sprite/.goodflows/runs/<uuid>/results/logs/ci/<run>` is
`results/logs/ci/<run>` in the bundle. A collection killed midway can leave a
private `.collect-*` staging directory. GoodFlows never publishes it, and the
next collection into the same directory removes it.

The remote source has no `.git`. While it builds the archive, the controller
records the snapshot's provenance. That provenance is the `HEAD` commit, the
branch when `git symbolic-ref` reports one, and `dirty`. The controller sends
these values in the submission header and keeps them in the control record.
The remote runner writes that `source` into the root's and every child's
history file, test-report records and run record. Jobs and hooks never see
these values, and neither `pass_env` nor argv carries them.

`dirty` describes the archive, not `git status`. It is true when a tracked
file's shipped bytes or executable bit differ from `HEAD`, when a tracked file
is missing (deleted, or excluded from the snapshot), or when the archive ships
a nonignored untracked file. Ignored files and `.git` do not count. GoodFlows
hashes the shipped bytes without Git filters. Files whose checkout differs
from their blobs therefore make `dirty` true even in a clean checkout. These
include Git LFS files, files with another clean filter, and files with
line-ending conversion such as `eol=crlf`. A detached `HEAD` has no `branch`,
and GoodFlows does not use GitHub Actions variables in its place. When
GoodFlows cannot read `HEAD`, there is no `source`.

### Reporting

Remote jobs never receive the publisher key. The remote run collects and
finalizes its spool with publication unavailable. After collecting, an
attached controller publishes best-effort with its own credentials. It
publishes only when the spool matches this invocation's run, project,
workflow, profile, jobs and partitions, and configured credential variable
names. The controller refuses a mismatch without looking up any other
variable.

The controller publishes a called workflow's spool too, after the same check
against the child's own run id and target workflow. It does so only when the
child's id is a canonical UUID and a summary of this run (the root's, or an
accepted caller's) lists it as a call's `child_run_id`. Publication happens
after collection. Child spools go first, in the order their summaries arrived,
then the root's, and the root report comes last. Unlike in a local run, a
child's upload lines therefore follow the child's own report (and any later
output) rather than preceding it. The controller publishes child spools only
when it received the root summary, which lists the root's calls. Otherwise,
replay one explicitly with `goodflows reports upload`.

Publication never changes the workflow's exit code. `sprite collect` never
publishes. Replay a collected spool explicitly with
`goodflows reports upload <bundle>/results/reports/<uuid>`, and only for
invocation data you trust.

With `[reporting] history = true`, each remote invocation freezes its run
record (`run-record.json`) in its own spool, without a key, and sends nothing.
This applies to the root and to every called workflow. Its relayed
`run_upload_pending` event says `credentials_unavailable` because the record
waits for the controller. After collection, the controller publishes the run
records with the reports, each after its spool's reports, with child spools
first and then the root's. The `reports_upload_summary` that follows each
spool gives the record's final state. The controller refuses a spool that
lists a run record, and sends nothing from it, unless the controller's own
configuration sets `[reporting] history = true`. A remote config that turns
the upload on therefore cannot make the controller send command text. A
detached run's record waits in the collected bundle for `reports upload`.
Delivery never changes the exit code.

A remote record's `source` is the snapshot provenance described above. It
holds `commit`, the archive-derived `dirty`, and `branch` only when git
reported a branch. Its `host` is the Sprite's, and its `duration_ms` is the
remote invocation's. The remote runner's environment is the minimal base plus
forwarded values. The record therefore has `ci` only when `[sprite] pass_env`
lists `GITHUB_ACTIONS` and the other GitHub variables the README names:

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

GoodFlows keeps Sprites, caches and run directories after success, failure,
cancellation and disconnect. It never prunes remote runs automatically, and
retained storage keeps costing money. Remote storage only grows. Each run
keeps `~/.goodflows/runs/<uuid>/` on the Sprite. That directory holds a full
copy of the snapshot (`source/`, about the uncompressed size of the
`sprite_snapshot` files) plus its events, hook output and `results/`. Each
distinct GoodFlows build used with the identity keeps its runner under
`~/.goodflows/runners/`, and your hooks' state directory grows as they decide.

GoodFlows removes none of this after a run, because recovery and collection
read the run directory and the plan deliberately leaves automatic remote
retention out. To clean up, collect what you need and destroy the identity's
Sprite. The next run creates and prepares a fresh one. You can delete files
inside the Sprite yourself (for example old `runs/<uuid>/source/` directories
of finished runs), but GoodFlows does not do it or manage it.

Remove an identity explicitly:

```bash
goodflows sprite destroy T-1          # asks for confirmation on a terminal
goodflows sprite destroy T-1 --yes    # automation
```

Destruction verifies ownership and refuses while a run is active or
unresolved, so cancel or reconcile first. It blocks new runs during the
operation and checks the resource id again immediately before deleting. It
never deletes a same-named replacement after an ambiguous acknowledgement.
Dependency state and uncollected results are lost permanently. Sprite and
publisher credentials stay on the controller throughout.

While a destroy is between its ownership check and confirmed deletion,
`sprite status` shows `destroying: true` ("destruction in progress"), and
GoodFlows refuses new runs. Each destroy holds its own claim on that marker,
so concurrent destroys never clear each other's claims. After the delete call,
the destroy lists the Sprite again every 2 s for up to 60 s, because the
platform may finish a delete after accepting it. The possible outcomes are:

- If the Sprite disappears, it is destroyed (exit 0).
- If the platform definitely refused the delete call (an
  `API error (status 4xx)` other than 408) and the same Sprite is still listed
  after 60 s, nothing was deleted. The destroy drops its claim, and the
  identity accepts runs again (exit 3).
- The delete call may have succeeded, or failed without a definite refusal. A
  failure without a definite refusal is a transport error, a timeout or a
  server error, any of which may hide an accepted delete. If the Sprite is
  then still listed or cannot be listed, the deletion may still be in
  progress. The claim stays, and runs stay refused (exit 3). Check
  `goodflows sprite status <identity>`. Once the Sprite is gone, it reports
  `absent`. If the Sprite is still there, run
  `goodflows sprite destroy <identity> --yes` again.
- If a failure happens before the delete call (the re-check listing fails,
  the resource changed, or this destroy's claim is no longer there), the
  destroy drops the claim (exit 1 or 3) and deletes nothing.

Two cases keep a claim on purpose:

- The destroying controller was killed or hung in that window. Its claim
  stays until a later `goodflows sprite destroy <identity> --yes` finds it
  older than 15 minutes and removes it. That later destroy also deletes the
  Sprite if it can. A destroy gives up after 10 minutes of its own (monotonic)
  time. A suspended controller host does not count that time, but the claim's
  age on the Sprite keeps growing. So each destroy checks, just before its
  delete call, that its claim still exists, and deletes nothing if the claim
  was swept. A small window remains between that check and the delete call.
  There is one more gap. Suppose the platform accepted a destroy's delete, and
  that destroy's stale claim has since been swept. A later destroy that the
  platform refuses then readmits runs, even if that earlier delete is still
  finishing.
- The platform reported another or no organization during the destroy
  (organization mismatch). GoodFlows then runs nothing more in that Sprite,
  not even the cleanup. If that happens before the delete call,
  `sprite destroy` exits 1, like any organization mismatch. If it happens
  while re-listing after the delete call, it exits 3, because the delete may
  have happened. Fix the credential by using one for the requested
  organization, or by setting `SPRITE_ORG` to the credential's organization.
  Then run `goodflows sprite destroy <identity> --yes` again.

An abort that dies between removing its claim and the marker directory leaves
an empty marker. `goodflows sprite status <identity>` removes it.

The ownership check can end in a definite refusal (an unowned or differently
scoped Sprite, or one with no GoodFlows runner), which exits 1. An
organization mismatch also exits 1. If the check fails in any other way, such
as a timeout, a transport error or a lost reply, the destroy deletes nothing
and exits 3. The check may have reached the Sprite and written a claim whose
token only the Sprite knows. Such a claim keeps refusing new runs
(`sprite status` shows `destroying: true`). It stays until a
`goodflows sprite destroy <identity> --yes` run 15 minutes or more later
sweeps it, and that destroy also deletes the Sprite if it can. Running destroy
again sooner deletes the Sprite as well, but leaves the old claim until it is
stale.

You cannot clear such a claim without destroying. `sprite status` deliberately
does not sweep old claims, because a claim older than 15 minutes can still
belong to a delete in progress. That delete may be an accepted delete whose
claim is kept on purpose, or a suspended controller between its claim check
and its delete call. Readmitting runs then could start work on a Sprite that
is about to disappear.

Ctrl-C (or SIGTERM) during `sprite destroy` exits 130 (143). The result
depends on when it arrives:

- At the confirmation prompt, or before the ownership check, nothing has
  changed.
- The ownership check itself is never interrupted midway. If it does not
  finish within its 2-minute bound, the destroy exits 3 (see below).
- After the ownership check and before the delete call, the destroy removes
  its own claim. Nothing was deleted, and runs are admitted again. If that
  removal fails, the output says so and the claim stays.
- Once the delete call has been issued, the claim stays, because the delete
  may be in progress, and new runs stay refused. Check
  `goodflows sprite status <identity>` and run
  `goodflows sprite destroy <identity> --yes` again.

## Dogfooding GoodFlows

`examples/sprites/goodflows.toml` runs this repository's own checks on a
Sprite with two scripts. `scripts/sprite-setup.sh` installs rustup and Rust
1.88.0 into `$GOODFLOWS_SPRITE_STATE_DIR`, and it detects a rustup wrapper
that is not a real installation. `scripts/sprite-resume.sh` verifies the
toolchain and reuses the persistent Cargo cache without rebuilding the
environment. Run it remotely with:

```bash
goodflows run --config examples/sprites/goodflows.toml --sprite "$THREAD_ID" --json
```

The same file also runs locally
(`goodflows run --config examples/sprites/goodflows.toml`). Without
`GOODFLOWS_SPRITE_STATE_DIR`, the jobs use the installed toolchain. Remotely,
runs reuse the Cargo registry and dependency builds in the state directory.
Each run has a new source path, though, so Cargo rebuilds the workspace crate
itself every time. Expect subsequent remote runs to be slower than a warm
local tree.
