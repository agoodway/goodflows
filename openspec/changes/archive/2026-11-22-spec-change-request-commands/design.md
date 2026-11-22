# Design

## Context

See proposal.md. The commands live in `src/change_requests/` (`mod.rs` for clap and handlers, `api.rs`, `git.rs`, `markdown.rs`, `verify_run.rs`) and `src/output/change_requests.rs`. `tests/change_requests.rs` drives them against a mock server, and the `verify-goodflows-cli` skill's recipes drive them against a live GoodFlows Web server.

## Decisions

### D1. Specify the client, not the server

Each requirement states what goodflows sends, prints and exits with. Server rules (validation, verdict rules, permissions) stay in goodflows_web's specs. A requirement names a server response only where the CLI reacts to it, such as the `409` rewrites or the `410` events cursor.

### D2. Shared rules live in `change-requests`

The reporting config, key rules, `--number` versus the current branch, `--json` events and error message formats are stated once in `change-requests`. Other capabilities refer to it.

### D3. Capability names follow goodflows_web

Using the server's capability names keeps a client and server change for the same feature easy to pair.

### D4. Code wins over docs

Where the code, `--help`, README or recipes disagree, the specs follow the code, and the disagreement is listed below so it can be fixed separately.

## Found while specifying

The specs describe the code below as it is. Each item needs its own change, which may update these specs.

### Docs that disagree with the code

- `cr path --help` says it "needs no server", but it loads the config like every `cr` command except `validate`: without `goodflows.toml` it exits 2 with `no config found`.
- `CR_EXAMPLES` says failures exit 1 and bad arguments 2. A missing config or a run flag such as `--env` also exits 2, and an interrupt exits 128 plus the signal number.
- README and help say writes need an `sk_` key. The CLI sends any key; only the server's `403` enforces it.
- `cr open` ignores `--title` and `--target` when it reuses an existing change request; the help mentions this only for `--title`.
- `cr runs upload --help` and the README list `branch` as required in `run.md`; the code sends null when it is missing.
- Help, README and the recipe say a diff set ending `+dirty` is refused; the code refuses any set whose `revision` ends `+dirty`.
- `cr scenarios diff --source diff` uses the baseline as `--from` only when it is below `--to`; otherwise the server compares with the previous version.
- `--follow` help says "30-second long-polls", but an explicit `--wait` overrides that.
- The review-and-events recipe says a `410` suggests starting without `--after`; the CLI says `read with --after <latest_cursor>`.
- The review-policy recipe says the server parses the TOML; `policy set` parses it locally and sends JSON. The recipe also reads `.version` and `.document` from `policy get --json`, where the event has `.policy.version` and `.policy.policy`.

### Behavior that looks like a bug

- `refuse_anchors` (`markdown.rs:106`) rejects plain text such as `reach: Login & signup` as a YAML anchor, in `cr validate`, `cr open` and `cr claims`.
- `evidence_path` (`verify_run.rs:505`) drops the first three parts of any `.verify/...` path without checking that they name this run folder.
- `cr runs upload` looks up the branch's change request after committing the run, so it can exit 1 when the run was stored.
- Some failures name an internal label instead of the command: `show failed: …` for `cr status`, `runs upload failed …` without `cr`.
- `cr answer` argument checks exit 1, not 2, and run after the branch lookup requests.
- Under `events --follow`, a nonempty page with no integer `next_cursor` prints the same events on every poll, and a `410` with `latest_cursor` null ends the loop with a plain error.
- `cr lease release` prints `state: released` whatever the server returns.
- A non-UTF-8 `run.md` is reported as `<folder> has no run.md`.
- `set_cr` rewrites the first front matter line starting `cr:`, even one that is not a top-level key.
