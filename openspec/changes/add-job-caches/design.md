# Design

## Context

See proposal.md for why. Facts that shape this design:

- goodflows has no `${{ }}` string templating. Conditions and `outputs` are whole expressions in the scalar-only language in `src/expression.rs`, which has no string concatenation and no functions. A GitHub-style key string such as `plts-${{ hashFiles('**/mix.lock') }}` would need both.
- Jobs run in the working tree (local) or in a fresh per-run source tree (Sprite). Locally, a path the job writes is already present on the next run in the same checkout. A cache only helps when the tree is new.
- On Sprites, docs/sprites.md says controller credentials, including the reporting key, never reach the Sprite, and that GoodFlows never manages caches.
- `tar`, `flate2`, `sha2` and `reqwest` are already dependencies. The Sprite snapshot code (`src/sprite/snapshot.rs`) already has rules for safe archives: no special files, no external symlinks, and path checks.

## Goals / Non-Goals

**Goals:**
- Declare a cache next to the job that uses it, with GitHub Actions semantics: exact key, fallback restore, save only on a passing job with a missed exact key, and immutable entries.
- One local store per project that every checkout and Sprite run shares, plus an optional shared remote store.
- A cache can only speed a job up. It never fails a job and never changes its status.

**Non-Goals:**
- Skipping jobs whose inputs are unchanged (Turborepo/Nx-style task caching). That is a different feature, and this design does not rule it out.
- Content-addressed or incremental storage. Each entry is one archive.
- A GitHub Actions cache-service backend. That API (twirp v2, `ACTIONS_RESULTS_URL`) is undocumented for third parties.
- Absolute paths outside the home directory, such as `/usr/local`. Workdir and `~/` paths cover dependency caches, build output and compiler caches.
- Multipart uploads. One entry is at most 5,000,000,000 bytes, under R2's single-request limit of 4.995 GiB.

## Decisions

### Config shape

```toml
[cache]                       # optional; without it only the local store is used
remote = "goodflows"          # uses [reporting] project and url_env
token_env = "GOODFLOWS_CACHE_KEY"  # default; when unset or empty, the [reporting] token_env is used
scope = "env.GITHUB_HEAD_REF ?? env.GITHUB_REF_NAME"  # optional; default: the Git branch
write = true                  # false: restore only (for example, untrusted PR builds)
local = true                  # false: remote only, no local copy
local_bytes = 5_368_709_120   # local store cap, LRU-pruned (default 5 GiB)

[environments.pr.cache]
write = false

[[jobs]]
name = "dialyzer"
depends_on = ["versions"]
command = "mix dialyzer"

[[jobs.caches]]
name = "plts"
paths = ["priv/plts"]
key.files = ["**/mix.lock"]
key.values = ["needs.versions.outputs.otp", "needs.versions.outputs.elixir"]
fallback = true               # default true
save = "passed"               # or "always"
timeout_secs = 300            # default; bounds the restore and the save separately
```

- `name` follows the shared name rule and is unique within the job. Two jobs may use the same name. Then they share entries, which is how a `deps` job and a `test` job can share `_build`.
- `paths` are globs relative to the job's workdir that must stay inside it, checked the same way as workdirs. There is at least one.
- `paths` may also start with `~/` for caches in the home directory, such as `~/.cargo/registry`, `~/.npm`, `~/.cache/pip`, `~/.m2`, `~/.gradle`, `~/go/pkg/mod` and `~/.hex`. A home path must name a directory below the home directory and must not be or contain `.ssh`, `.aws` or `.gnupg`. On a Sprite, `~` is the Sprite user's home.
- `key.files` are globs relative to the workdir. goodflows hashes the sorted list of matching paths and their contents. When nothing matches, the hash is the empty digest. This is not an error, matching `hashFiles`.
- `key.values` are expressions evaluated with the job's condition lookup: `env.*` and `needs.*`, but not `steps.*`, because no step has run yet. Each value must be a scalar. A runtime error makes the cache miss with a warning.
- `key.unique` are expressions like `key.values`, hashed into the key but not the partial digest. They make rolling caches work: with `key.unique = ["env.GITHUB_SHA"]`, every commit gets a new key, restores the previous commit's entry as a fallback, and saves its own (GitHub's `key: ccache-${{ github.sha }}` with `restore-keys: ccache-`). The server's LRU quota and `local_bytes` bound the growth.
- Matrix dimensions other than OS and architecture, such as the Node or Rust version, must be in `key.values`, as in GitHub.
- Platform (`std::env::consts::OS`/`ARCH`) is always part of the key, so users never add `runner.os` by hand.
- Caches are allowed on command and step jobs, including partitioned ones (each instance restores and saves on its own). They are rejected on workflow-call jobs, which run no command of their own.

Alternative: add string interpolation and a `hash_files()` function to the expression language and take a GitHub-style `key = "..."` string. Rejected for the first version because it is a larger language change, and a structured key cannot accidentally leave out the platform. Users who want a readable key can see it in `goodflows list`.

### Keys

```
partial = sha256(cache-format-version, name, os, arch, values as canonical JSON)
key     = sha256(partial, files digest, unique values as canonical JSON)
```

An entry is stored under `key` and tagged with `partial`. A fallback restore picks the newest entry with the same `partial`. That is the `restore-keys` behavior without a key prefix string.

### Lifecycle (in `run_job`)

1. The condition passes and the key is computed.
2. Restore. Try the exact key in the local store, then the exact key in the remote store, then (if `fallback`) the newest partial match locally, then remotely. A remote hit is also written to the local store. A restore writes only when at least one declared path is missing, and then replaces every declared path with the entry. When every path exists, goodflows keeps the tree and looks nothing up. Build tools already handle a stale tree, and a rerun in the same checkout never re-extracts the same archive. A key stamp in `.goodflows/cache-stamps/` records which key the tree was built for. A present tree whose stamp matches the key reports `exact` from tier `workdir`; any other present tree reports `present`. Expressions in the job's steps and `outputs` can read `caches.NAME.hit`, which is true for an exact restore or a matching stamp (GitHub's `cache-hit`). For example, a step can set `condition = { expression = "!caches.deps.hit" }` so `mix deps.get` runs only on a miss. The job's own `condition` cannot read it, because the condition runs before the restore. Config validation rejects a `caches.NAME` that the job does not declare. No environment variable is added. Shell commands can test the directory themselves.
3. The command or steps run. Restore and save time is reported separately and does not count against `timeout_secs`. Each has its own bound, the cache's `timeout_secs` (default 300 s). A 5 GB entry at 300 s needs about 16.7 MB/s, so large caches on slow links should raise it; the server keeps an upload reservation open for 2 h.
4. Save, when the exact key missed, `write` is enabled, and the job's status matches `save`. The default `save = "passed"` saves only when the job passed. `save = "always"` also saves when the job failed, for dependency caches worth keeping when tests fail. A job that timed out or was cancelled never saves, because its paths may be half-written. Before archiving, check whether the exact key is already stored (local store, then a remote lookup), and skip with reason `exists` when it is. Archive the paths (tar + gzip), then write the local entry by atomic rename. Whenever the job ends with a status that `save` accepts, set the stamp to the key, whether or not an entry was written. When the remote store is configured, upload it. The save runs in the job's slot after its status is final, so dependents may start before it finishes, but the run does not end until every save has finished or reached its bound.

Partitioned instances and parallel jobs that compute the same key race to save. Locally the first rename wins and later ones are discarded. Remotely, a `409` from reserve counts as success.

### Archives

Each archive is a tar.gz with a JSON header entry: format version, key, partial, the list of paths, and the uncompressed size. Entries are stored under two roots, `workdir/` and `home/`, so a restore puts each path back where it was declared. On save, goodflows refuses special files and symlinks that point outside their root (the workdir, or the declared `~/` directory), and copies internal symlinks as symlinks. On restore, before anything is written, it refuses absolute paths, `..`, entries outside the declared paths, and archives whose SHA-256 does not match the store's metadata. The cap is 5,000,000,000 bytes compressed per entry, and an entry over the cap is not saved (`cache_save_skipped`, reason `too_large`).

### Local store

The store is at `$XDG_CACHE_HOME/goodflows/<project-digest>/` (macOS: `~/Library/Caches/goodflows/...`). The digest identifies the project by its normalized Git remote URL, so every clone and worktree of a repository shares one store. Without a remote, it uses the canonical repository path. Outside Git, it uses the canonical directory of the config file. On a Sprite the store is `$GOODFLOWS_SPRITE_STATE_DIR/caches/`. The directory is private (0700), and its entries are files named by key with a `.meta` sidecar holding the partial, the sha256, the size and the last-access time. Pruning reuses the LRU approach in `src/retention.rs` and keeps the store under `local_bytes`.

### Remote store protocol

goodflows_web specifies this protocol in its `add-remote-cache-storage` change. Everything is under `/api/v1/projects/:project/caches` with Bearer auth and `{"data": …}`/`{"errors": …}` envelopes. Keys are unique per project and scope, so a branch that saved key K first never serves its K to main.

| Request | Meaning |
|---|---|
| `GET entries/:key?scope=S` | `200` with `key, partial, name, scope, size, sha256, created_at, download_url, download_url_expires_at` from scope S or else the default branch's scope, or `404` |
| `GET entries?partial=P&scope=S` | The newest committed entry in the first scope that has one (S, then the default branch), or `404`. A newer default-branch entry does not beat an older one in S |
| `POST entries/:key` with `{name, partial, scope, size, sha256}` | `201 {upload_id, upload_url, upload_headers, upload_url_expires_at, expires_at}`. `409` means the key is committed or reserved in that scope (count as saved). `403` means read-only, `413` means over 5,000,000,000 bytes, `507` means the quota is full |
| `PUT upload_url` with exactly `upload_headers` | The headers (length, `application/gzip`, base64 `x-amz-checksum-sha256`) are signed into the URL, so the object store rejects a wrong size or hash |
| `POST uploads/:upload_id/commit` | `200` makes the entry visible after the server checks the object. `422` means a mismatch and drops the reservation. A retried commit returns `200` |

The CLI never sends `Authorization` to presigned URLs, and it checks a download's sha256 before unpacking. Reads send the run's scope (see Trust and scoping) when it has one. Without one, the server reads only the default branch's scope, and remote saves are skipped (`cache_save_skipped`, reason `no_scope`). A `503` (no bucket configured) turns the remote store off for the rest of the run, with one `cache_remote_unavailable` event. `413` and `507` are `cache_save_skipped` with reasons `too_large` and `quota`.

Alternative: speak S3 directly. Out of scope: the GoodFlows server is the only remote backend.

### Trust and scoping

A shared cache is a code-execution channel: a poisoned `_build` runs on the next machine that restores it. Rules:

- **Scope.** `[cache] scope` is an expression over `env.*`. When it is unset, or evaluates to null or an empty string, the scope is the Git branch from `reporting::source`. This lets detached checkouts, such as GitHub Actions pull requests, save to their own branch's scope (`env.GITHUB_HEAD_REF ?? env.GITHUB_REF_NAME`; `GITHUB_HEAD_REF` is empty on push events, which then fall back to `GITHUB_REF_NAME`). The scope is a label, not an authority: what a run may write is decided by its key's binding on the server, so a run that sets `scope = "'main'"` with a feature-branch key gets `403`. The server compares scopes as exact byte strings of 1 to 256 UTF-8 bytes with no control characters, and never checks them as Git refs. A bound read key gets `403` for any other scope, so when a scoped lookup gets `403`, the CLI drops the scope for the rest of the run and reads only the default branch. An orb on a feature branch with a main-bound key still gets main's caches. The server enforces the lookup order (this branch, then the default branch, never another feature branch) and owns `default_branch`.
- **Write gate.** `write = false` (settable per environment profile) makes a run restore-only. A `403` on reserve (a read-only key) is treated as read-only, with one warning per run, not one per job. `pk_` keys cannot use caches. A run whose key is `pk_` gets one `cache_remote_unavailable` event.
- **Integrity.** Every download is checked against the committed sha256, and extraction follows the archive rules above.
- **Credentials.** The cache key comes from `[cache] token_env` (default `GOODFLOWS_CACHE_KEY`). When that variable is unset or empty, the `[reporting]` key is used. A separate variable lets an orb hold a read-only `ck_` key without breaking run-history uploads, which need an `sk_` key.
- **Purge.** `sk_` keys can delete entries by key or by name, scope and partial. A future `goodflows cache delete` can wrap this. The CLI design does not need it.

### Sprites

The local store lives in the state directory, so Sprite runs get caching with no remote configured and no credentials on the Sprite.

For the remote store, the controller keeps its `sk_` reporting key. Before the remote run starts, it mints a `ck_` cache key (`POST /projects/:project/cache-keys`) with `read_write` access (or `read` when `write = false`), bound to the run's scope, with a TTL covering the run. The controller resolves the scope from its own environment, because the Sprite's environment is minimal. The controller forwards the minted key to the Sprite through the same private input frame as `pass_env` values, never in argv, files or events, and revokes it (`DELETE /cache-keys/:id`) after collection or cancellation. The key can touch only caches in this project and this branch, so a hostile job on the Sprite cannot forge history or reports and cannot write main's caches from a feature branch. Minting needs an `sk_` controller key. A run without a scope gets an unbound `read` key, which reads only the default branch. When minting fails, the run continues without the remote store and prints `cache_remote_unavailable`. Users do not configure a cache key or list one in `pass_env`.

Alternative: the controller proxies every transfer over `sprite exec`. Rejected because each archive would cross the controller's link twice.

### Example: warming caches in orb setup

Amp keeps a separate project snapshot for each orb size. The first orb of each size runs `.agents/setup` from scratch, and setup runs again on top of the snapshot after each new base-branch commit. A setup script can warm a cache by running a workflow:

```toml
[[workflows.orb-setup.jobs]]
name = "plt"
command = "mix dialyzer --plt"

[[workflows.orb-setup.jobs.caches]]
name = "plts"
paths = ["priv/plts"]
key.files = [".tool-versions", "mix.lock"]
```

`.agents/setup` runs `goodflows run orb-setup` after installing the toolchain and dependencies. The main workflow's dialyzer job declares the same cache, so they share entries. For the first orb of a size, the PLT comes from the remote store when CI or another orb already built it. On a later setup over a snapshot, the PLT is already present, so goodflows keeps it, and the stamp in the snapshot's `.goodflows/` says whether it matches the current key. Orb size is not part of the key; all Amp orbs are Linux x86_64, so their keys match.

Orbs should get a long-lived read-only `ck_` key bound to the default branch (`{"access":"read","ttl_seconds":null,"scope":"main","name":"orbs"}`) as the Amp project secret `GOODFLOWS_CACHE_KEY`, and CI on the default branch should be the writer. A snapshot is shared with project members, and setup receives the project's secrets. A writable key there lets any orb's experimental code save into main's scope. The local store also ends up in the snapshot, which stores each PLT twice. Profiles can set `[environments.orb.cache] local = false` to skip it.

### Output

- Text: one line per cache, `cache plts: restored (exact, local, 182 MiB, 3.1s)`, `cache plts: saved (remote, 180 MiB)`, `cache plts: miss`.
- JSON: `cache_restored {job, partition, cache, result: exact|fallback|present|miss, tier: local|remote|workdir|null, bytes, duration_ms}`, `cache_saved {..., tiers}`, `cache_save_skipped {..., reason: hit|exists|not_passed|read_only|no_scope|too_large|quota|error}`, `cache_remote_unavailable {reason}`, `cache_warning {..., message}`.
- `goodflows list` shows each job's caches with their paths and key inputs.
- History records get per-job cache results, so hit rates are visible on the server.

## Risks / Trade-offs

- [Poisoned shared cache] → Branch scoping, the write gate, read-only keys, and checksums on every download. A trusted writer can still produce a bad cache, so document `goodflows cache delete` or a key bump (`key.values = ["'v2'"]`).
- [Restore replaces paths the user edited locally] → Restore happens only when a job runs, and it replaces only the declared paths. Document that cached paths belong to goodflows while a job runs, the same way build directories do.
- [Secrets in caches] → Unlike the Sprite snapshot, cache archives are not scanned for credentials. A declared directory that holds a token is uploaded with it. Document this, and never declare credential directories (the home rules refuse the obvious ones).
- [Absolute paths in build output] → A key match does not mean the tool reuses the files. Cargo's `target/` and possibly Dialyzer PLTs record absolute paths, so a restore into a different checkout path (every Sprite run, an orb with a different toolchain location) can trigger rebuilds. Document installing toolchains at the same path everywhere, or putting the install path in `key.values`.
- [Rolling caches saved on every local run] → With `key.unique`, a local rerun keeps its tree but saves a new entry each time. Profiles can set `write = false` for local runs.
- [Disk growth] → `local_bytes` LRU cap locally, server policy remotely.
- [Restore slower than rebuild for small paths] → Durations are reported per cache, so users can see it and remove the cache.
