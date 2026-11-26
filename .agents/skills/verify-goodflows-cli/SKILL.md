---
name: verify-goodflows-cli
description: Builds this checkout's goodflows CLI and drives its change request commands (cr open, cr claims, cr push, cr scenarios, cr runs upload, cr verdict, cr review, policy, features sync, events) end to end against a fresh, isolated GoodFlows Web server, in a scratch git checkout, capturing proof under .verify/goodflows-cli/. Use to prove a CLI change that talks to the server works against the real server, not just the mock in tests/change_requests.rs. Replaces goodflows_web's scripts/e2e-cli-smoke.sh for change requests.
---

# Verify the goodflows CLI against GoodFlows Web

The goodflows CLI's server surface is a set of commands a coding agent runs in a git checkout: `cr …`, `policy …`, `features sync` and `events`. They read the checkout's git state and files, call `/api/v1/projects/<project>/…` with the `[reporting]` URL and key, and print text or, with `--json`, one JSON event. `cargo test --test change_requests` covers them against a mock; this skill drives them against the real server.

The server is goodflows_web. This skill does not start it by hand: it calls goodflows_web's own `verify-goodflows` launch, which gives a free port in 4400–4499 and a freshly created, migrated and seeded database, and its cleanup, which stops that server and drops that database.

Everything goes through one helper. Call it by path from this repository's root:

```bash
C=.agents/skills/verify-goodflows-cli/scripts/control-goodflows-cli
```

Read [`features/README.md`](features/README.md) before driving anything. Each feature file there is the recipe for that feature.

## Launch

Preconditions:

- A goodflows_web checkout at `GOODFLOWS_WEB_DIR` (default `../goodflows_web`) whose `.agents/setup` has run, so Elixir 1.19.4, OTP 28 and PostgreSQL on `localhost:5432` exist. Its revision decides which server you test; record it.
- Rust from this repo's `.agents/setup`, `jq`, `curl`, and `agent-browser` (goodflows_web's launch checks for it).

```bash
export GOODFLOWS_WEB_DIR=/path/to/goodflows_web
eval "$($C launch)"     # exports RUN_ID, GF_URL, GF_RUN_DIR, GF_CHECKOUT
```

Launch does this:

1. Creates `.verify/goodflows-cli/$RUN_ID/` with `run.md`, and adds `/.verify/` to `.gitignore` when it is missing.
2. Runs `cargo build`; the binary is `target/debug/goodflows`.
3. Starts versitygw 1.11.0 (downloaded once to `~/.cache/verify-goodflows-cli/` and checked against its SHA-256) on a free port in 4500–4599, with a `verify-artifacts` bucket on local disk, and exports `CACHE_S3_*` so the server stores verify-run evidence there. moto does not return `x-amz-checksum-sha256` on HEAD, which goodflows_web checks before committing a run, and MinIO no longer publishes binaries.
4. Runs goodflows_web's `control-goodflows launch`. A cold Phoenix compile takes minutes; it waits up to 180 s once compiled.
5. Mints an `sk_` key for the seeded owner of `Acme Corp` with `Accounts.create_api_key/2`, the function the dashboard's API Keys page calls. This is setup: the API Keys feature belongs to goodflows_web's own skill.
6. Creates project `verify-cli-<time>` with `POST /api/v1/projects`. Its `default_branch` is `main`.
7. Makes a scratch checkout under `/tmp/verify-goodflows-cli-$RUN_ID.*`: `main` with one commit pushed to a bare `origin`, and `feat/verify` one commit ahead, with a `goodflows.toml` whose `[reporting] project` is the new project.

It is ready when it prints the `export` line. Each launch has its own server, database, project and checkout, so runs do not collide; they share `target/` and goodflows_web's `app/_build`, so do not launch while another agent is compiling either checkout.

## Doctor

Run it first, and again whenever anything looks off:

```bash
$C doctor
```

It is read-only. It runs goodflows_web's doctor (server pid, port ownership, `/users/log-in`, seeded database, its commit) and then checks that the CLI binary is newer than `Cargo.toml`, that this checkout is still at the launched commit, that the key reads the project (HTTP 200), that the scratch checkout exists, and that versitygw is serving. Any `FAIL` means the instance is not worth driving: clean up and launch again.

## Drive

```bash
$C cli open-before-code cr open --change add-login --title "Add login" --json
$C git log --oneline -1                  # git in the scratch checkout
$C api cr-1 GET /api/v1/projects/{project}/change-requests/1
```

- `cli <name> <args…>` runs `target/debug/goodflows <args…>` in the scratch checkout with `GOODFLOWS_URL` and `GOODFLOWS_API_KEY` set, and saves `cli/<name>.cmd`, `.stdout`, `.stderr` and `.exit`. It prints the exit code and both streams. Use `--json` when you assert on fields (`jq` the `.stdout` file); run the same command without it once per feature to capture the text output a person reads.
- `git <args…>` runs git in the scratch checkout, to commit, edit and check out branches between commands.
- `api <name> <METHOD> <path> [body]` is the second view: it reads what the server stored. `{project}` in the path becomes the run's project. It saves `api/<name>.request`, `.status` and `.json`.

Write files into the scratch checkout (claims, scenario sets, run folders) with ordinary shell redirection into `$GF_CHECKOUT`. The feature files give the exact content.

## Evidence

Every run writes to `.verify/goodflows-cli/$RUN_ID/` at this repository's root; `.verify/` is gitignored. `run.md` holds the full commit SHA, the branch, a clean or dirty working tree with the changed files, the start time, the server URL and goodflows_web revision, and the project. goodflows_web's own run folder (named in `run.md`) keeps the server log, so a `422` can be traced to the server's reason.

Record each sub-feature you drive, then the outcome:

```bash
$C record cr-create "cr open --claims (cr: null)" pass cli/open.stdout api/cr-1.json
$C finish pass "one-line summary"      # or fail / partial
```

Proof standards:

- Drive the CLI the way the `gs-change-request` and `gs-qa-scenarios` skills do: real commands in a git checkout, never hand-built requests to the routes the CLI is supposed to call. `api` is only for reading back what was stored.
- Capture the command and the resulting state: the CLI's output and exit code, and a server read of the stored record (the change request's `head`, the claims document byte for byte, the scenario set's `digest`, the verify run's `status`).
- Check side effects outside the server too: `cr open` writes the number into the claims file's `cr`, `cr push` pushes to the bare origin (`$C git ls-remote origin`), `--json` stdout stays one JSON event per line.
- A server `4xx` is a finding, not noise. Quote the CLI's error and the server log line in the feature's result.
- Never paste the `sk_` key into `run.md` or a reply. It dies when cleanup drops the database, and cleanup blanks it in `instance.env`.

## Cleanup

```bash
$C cleanup
```

It runs goodflows_web's cleanup (closes its browser session, stops the server process group that launch started, drops the run database), stops versitygw and removes its bucket, removes the scratch checkout under `/tmp/verify-goodflows-cli-*`, blanks the key in `instance.env`, and appends a `Cleaned up` line to `run.md`. It never removes `.verify/`. Run it after failed attempts too. If the shell lost `RUN_ID`, pick it from `ls .verify/goodflows-cli/` and `export RUN_ID=<id>` first.

## Helpers

- `scripts/control-goodflows-cli` runs as shown above. With no arguments it prints its usage.
- Keep the map current with the `gs-maintain-verification-skill` skill as commands change.
