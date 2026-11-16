# Tasks

## 1. Configuration

- [ ] 1.1 In `src/config.rs`, add `caches: Vec<Cache>` to `Job` (`name`, `paths`, `key.files`, `key.values`, `key.unique`, `fallback`, `save`, `timeout_secs`) and the top-level `[cache]` block (`remote`, `token_env`, `scope`, `write`, `local`, `local_bytes`). Add `write`, `local` and `local_bytes` overrides to workflow and environment-profile settings, with the existing precedence. Use `deny_unknown_fields` like the other blocks.
- [ ] 1.2 Validate in `validate_jobs`/`validate_job` for every workflow:
  - cache names follow `valid_name`, unique per job;
  - `paths` are nonempty; workdir paths stay inside the workdir (reuse the workdir checks); `~/` paths name a directory below home and are not or do not contain `.ssh`, `.aws` or `.gnupg`;
  - `save` is `passed` or `always`;
  - `timeout_secs` is a positive integer;
  - workflow-call jobs have no caches;
  - `key.values` and `key.unique` parse and use `validate_expression` with no `steps.*` or `caches.*`;
  - `remote` requires `[reporting]`;
  - `token_env` follows `validate_credential_env_names` rules;
  - `scope` parses and reads only `env.*`.
  Add unit tests for each rejection, with errors naming the job and the cache, plus one accepted config. Verify with `cargo test --lib config`.
- [ ] 1.3 `goodflows list` (text and JSON) shows each job's caches with their paths and key inputs. Extend `tests/inspection.rs`.

## 2. Expressions

- [ ] 2.1 In `src/expression.rs`, add `Reference::Cache { name }` for `caches.NAME.hit`. In `src/config.rs`, allow it in step conditions and job `outputs`, and reject it in job conditions, in `key.values`, and when NAME is not declared by the job. Add parser and validation tests. Verify with `cargo test --lib`.

## 3. Keys and archives (`src/cache/`)

- [ ] 3.1 `key.rs`: the partial digest (format version, name, OS, arch, values as canonical JSON) and the key (partial plus a files digest over sorted relative paths and contents plus `key.unique`). Unit tests: a changed lockfile or unique value changes only the key, value order matters, no matching files is allowed, and a different OS or arch changes the partial digest.
- [ ] 3.2 `archive.rs`: tar.gz writer with a JSON header entry and `workdir/` and `home/` roots, and a verifying extractor. The extractor checks the sha256 first and refuses absolute paths, `..`, entries outside the declared paths, and external symlinks before writing anything. It extracts to a temporary directory and then swaps it into place. The writer refuses special files and external symlinks, and enforces the 5,000,000,000-byte cap. Symlinks may not leave their root. Tests cover each refusal, a hand-built malicious archive, and a round trip with internal symlinks and executable bits. Reuse the path rules in `src/sprite/snapshot.rs` where they fit.

## 4. Local store

- [ ] 4.1 `local.rs`: the store directory (XDG / macOS / `GOODFLOWS_SPRITE_STATE_DIR/caches`), the project digest from the normalized Git remote URL, the canonical repository path, or (outside Git) the config file's directory, 0700 permissions, entries with `.meta` sidecars, atomic first-writer-wins saves, the newest entry for a partial digest, and last-access updates. LRU pruning under `local_bytes` follows `src/retention.rs` and never deletes an entry that is being read. Tests: two worktrees share entries, concurrent saves of one key leave one entry, pruning order is correct, and a corrupt entry reads as a miss.

## 5. Runner lifecycle

- [ ] 5.1 In `run_job` (`src/runner.rs`), compute keys after the condition passes. When every declared path exists, keep the tree and report from the stamp in `.goodflows/cache-stamps/` (`exact` from `workdir` when it matches, `present` otherwise). Otherwise restore in the order exact local, exact remote, fallback local, fallback remote. Update the stamp after a restore and after a status that `save` accepts. Before archiving, skip with `exists` when the key is already stored. Each cache has its own bound, outside `timeout_secs`. Expose `caches.NAME.hit` to step and `outputs` evaluation. Save after the final status according to `save` and `write`, in the job's slot, and make the run wait for saves to finish. No cache failure may change a status or exit code.
- [ ] 5.2 Events and text lines in `src/output.rs`: `cache_restored`, `cache_saved`, `cache_save_skipped`, `cache_remote_unavailable` and `cache_warning`. Add cache results to the run-history record in `src/history.rs`, and update `src/agents_guide.md`.
- [ ] 5.3 Integration tests in a new `tests/caches.rs`:
  - miss then save, then an exact hit in a fresh copy of the tree;
  - a fallback into a missing path, and `present` over an existing one;
  - a rerun in the same checkout extracts nothing and reports `exact` from `workdir`, and a changed lockfile reports `present` and saves the new key;
  - `save = "always"` on a failed job, and no save after a timeout or cancellation;
  - `write = false` through a profile;
  - two partitions saving one key;
  - a step skipped by `!caches.deps.hit`;
  - a `~/` path restored into a fresh `HOME` and left alone when present;
  - a rolling cache with `key.unique` restoring the previous run's entry as a fallback and saving a new one;
  - a corrupt entry leaves the job passing with a warning;
  - a skipped job neither restores nor saves.
  Verify with `cargo test --test caches`.

## 6. Remote store

- [ ] 6.1 `remote.rs`: a client on the existing reqwest setup in `src/reporting/publish.rs`. Key resolution is `[cache] token_env`, then the reporting key. Exact and partial lookups send the scope from `[cache] scope`, else the branch from `reporting::source`, and omit it when neither yields one. Downloads use no `Authorization`, refuse redirects to other hosts, and check size and sha256. Saves reserve, `PUT` with exactly `upload_headers`, then commit. Status mapping: `409` counts as saved; `403` makes the run read-only after one warning; `413` and `507` are skip reasons; a `422` on commit is a warning; `503`, connection failures and timeouts turn the remote store off for the run. Responses are bounded and untrusted, like `history_client.rs`.
- [ ] 6.2 Tests against a local fake server (follow the fake servers in `tests/reporting.rs`). Cover each status mapping, a tampered download, that presigned requests carry no `Authorization`, `no_scope` on a detached checkout, a scope over 256 bytes falling back to the branch, a `403` on a scoped lookup retried without a scope for the rest of the run, a scope from `GITHUB_HEAD_REF`, an empty `GITHUB_HEAD_REF` falling back to the branch, one warning for a read-only key, and that the local entry survives a failed upload.

## 7. Sprites

- [ ] 7.1 In `src/sprite/`, when the selected run has the remote store enabled, the controller mints a `ck_` key with its `sk_` key before the remote run. It is `read_write` bound to the scope the controller resolves from its own environment, or `read` when writes are off or there is no scope, with a TTL covering the run up to 24 h. The key travels in the private forwarded-value frame and only the runner's cache client reads it, so it never reaches hook or job environments. The controller revokes it after collection, cancellation or reconciliation. When minting fails, the run continues with the local store and one `cache_remote_unavailable`. Add a deny entry so `pass_env` and configured env cannot name the cache `token_env` on remote runs.
- [ ] 7.2 Extend `tests/sprite.rs` with the fake Sprite CLI and a fake server. The minted key appears only in the input frame, never in argv, files, events, bundles or receipts. A job that dumps its environment finds neither key. The key is revoked after collect and after cancel. A `403` on minting falls back to the local store. The Sprite's local store is `$GOODFLOWS_SPRITE_STATE_DIR/caches`.

## 8. Documentation

- [ ] 8.1 README: a Caches section with config, keys, the restore and save rules, key stamps, `[cache] scope` with a GitHub Actions example, `caches.NAME.hit`, the local store and remote setup (cache keys, `token_env`, read-only CI and orb keys). Add examples for an Elixir PLT, Cargo (`~/.cargo/registry`, `~/.cargo/git`, `target`), npm (`~/.npm`) and a rolling ccache with `key.unique`. Note that matrix dimensions belong in `key.values`, that archives are not scanned for secrets, and that tools may rebuild output that records absolute paths when the checkout or toolchain path differs.
- [ ] 8.2 docs/sprites.md: replace "GoodFlows never discovers, mounts or copies caches" with declared caches and the per-run cache key, and keep hand-wired state directories as an option.
- [ ] 8.3 docs/ampcode.md: warm caches from `.agents/setup` with `goodflows run orb-setup`. Mint a long-lived read-only `ck_` key bound to the default branch (`{"access":"read","ttl_seconds":null,"scope":"main","name":"orbs"}`), store it as the project secret `GOODFLOWS_CACHE_KEY`, and let CI on the default branch write. Mention `[environments.orb.cache] local = false`.

## 9. Integration check

- [ ] 9.1 Run `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked` and `goodflows validate`. Run `openspec validate add-job-caches --strict`.
