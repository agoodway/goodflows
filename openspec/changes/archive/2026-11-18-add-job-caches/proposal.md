# Proposal

## Why

Jobs that build expensive artifacts, such as Dialyzer PLTs, `deps/` and `_build/`, Cargo's `target/` or `node_modules/`, rebuild them from nothing whenever the directory is missing. That happens on:

- every Sprite run, because each run gets a fresh `runs/<uuid>/source/` tree. Users work around it today by hand-linking caches from `GOODFLOWS_SPRITE_STATE_DIR` (see `scripts/sprite-setup.sh`), which docs/sprites.md says GoodFlows never manages;
- every new checkout or worktree, such as a fresh Amp orb, a second worktree, or a tree after `git clean -fdx`;
- every machine that has never built the project. A cache saved by one machine or CI run cannot help another, because nothing is shared.

GitHub Actions users handle this with `actions/cache`: a key built from the OS, tool versions and lockfile hashes, a path, restore before the job and save after it. goodflows has no equivalent.

## What Changes

- A job can declare `[[jobs.caches]]`. Each cache has a name, paths relative to the job's workdir or under `~/` (package manager caches such as `~/.cargo/registry` or `~/.npm`), and a key built from file hashes and expressions (`env.*`, `needs.*`). `key.unique` adds parts that change the key but not the fallback match, for rolling caches such as ccache. goodflows restores the cache after the job's condition passes and before its command or first step runs. When the exact key missed and the job passed (or failed, with `save = "always"`), goodflows saves the cache. Steps can read `caches.NAME.hit`.
- A miss on the exact key can fall back to the newest entry with the same name, platform and values. This is the `restore-keys` behavior. A restore never overwrites a path that already exists. A key stamp in `.goodflows/` tells whether an existing tree matches the key.
- Every run has a local cache store. It lives in the user's cache directory, so all checkouts of one project share it. Outside Git, the store is keyed by the config file's directory. On a Sprite it lives in the identity's state directory, where it survives between runs.
- A new top-level `[cache]` block can add a remote store, read through and written through behind the local one. The only remote backend is the GoodFlows server. It uses the `[reporting]` project and URL with a cache key from `[cache] token_env`, falling back to the reporting key. On Sprites, the controller mints a short-lived, branch-bound `ck_` key for each run. Long-lived environments such as orbs and laptops can hold a read-only `ck_` key bound to the default branch. The protocol allows the server to redirect to object storage such as S3.
- The remote scope defaults to the Git branch. `[cache] scope` sets it from an expression over `env.*`, so detached CI checkouts can save to their own branch.
- A cache never changes a job's result. A failed restore or save prints a warning event and the job runs as if the cache missed.
- New JSON events: `cache_restored` (exact, fallback, present or miss, plus the source tier and size), `cache_saved`, `cache_save_skipped`, `cache_remote_unavailable` and `cache_warning`.
- docs/sprites.md: "GoodFlows never discovers, mounts or copies caches" changes to describe declared caches. Hand-wired state directories keep working.

## Capabilities

### New Capabilities

- `job-caches`: declaring caches on jobs, how keys are built, the restore and save lifecycle, local storage and retention.
- `remote-cache`: the remote store, its protocol, scoping and trust rules, and credentials, including per-run keys for Sprites and read-only keys for orbs.

### Modified Capabilities

None. The repository has no base spec for Sprites. `job-caches` states where the store lives on a Sprite, and `remote-cache` covers Sprite credentials.

## Impact

- Code: `src/config.rs` (the `caches` field on `Job` and the `[cache]` block), a new `src/cache/` module (keys, archives, local store, remote client), and `src/runner.rs` (restore and save around `run_job`). Also `src/output.rs` for the events and `src/retention.rs` for local pruning.
- Code: `src/expression.rs` (a `caches.NAME.hit` reference) and `src/sprite/` (minting, forwarding and revoking the per-run key).
- Companion change in goodflows_web, `add-remote-cache-storage`: cache endpoints, R2 storage, `ck_` keys and `projects.default_branch`. The CLI's remote store needs it deployed. The local store works without it.
- Docs: README (a Caches section), docs/sprites.md (declared caches and the per-run key) and docs/ampcode.md (warming caches in `.agents/setup`).
- Users: opt-in. Configs without `caches` behave exactly as before.
