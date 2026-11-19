# Tasks

## 1. Dependencies and shared constants

- [ ] 1.1 In `Cargo.toml`, add `md-5 = "0.10"`, `base64 = "0.22"` and `tokio-util = { version = "0.7", features = ["io"] }`, and add `[profile.dev.package.md-5] opt-level = 3` next to the `sha2` entry. Verify `cargo build` succeeds and `Cargo.lock` gains only `md-5` (and its own dependencies, if any are new).
- [ ] 1.2 In `src/cache/remote.rs`, rename the ranged-download constants to path-neutral names (`PART_BYTES`, `PARTED_MIN_PARTS`, `PART_CONNECTIONS`, `PART_ATTEMPTS`), update their doc comments to cover uploads, and make `part_bytes()` reachable from `src/cache/mod.rs`. Verify `cargo test --test remote_cache` still passes unchanged.

## 2. Part MD5s while writing (`src/cache/archive.rs`)

- [ ] 2.1 Give `Counting` optional per-part MD5 state (part size, current hasher, finished base64 digests) that splits each written slice at part boundaries, finish the last part when the writer finishes, and add `part_md5: Option<(u64, Vec<String>)>` to `Written`. `archive::write` takes `parts: Option<u64>`. Unit tests: for part sizes that divide the archive exactly, leave a short last part, and exceed the archive size, the digests equal the MD5s of the file's slices read back from disk, and `None` yields `part_md5: None`. Verify with `cargo test --lib cache::archive`.
- [ ] 2.2 In `src/cache/mod.rs`, pass `Some(part_bytes())` to `archive::write` only when the session has an enabled remote store, and update the other callers (tests) to pass `None`. Verify with `cargo test --lib cache`.

## 3. Multipart reservation and part uploads (`src/cache/remote.rs`)

- [ ] 3.1 Extend the fake server in `tests/remote_cache.rs` to model multipart uploads as in design.md (Tests): `parts` on reserve, `upload_parts` with `content-length`/`content-md5`, part `PUT`s that enforce their headers (`403`) and body MD5 and length (`400`), and a commit that requires every part and checks the joined size and sha256 (`422`). Add the flags `no_multipart`, `single_url`, `flaky_parts`, `corrupt_part` and `short_parts`, and a helper listing part requests. Verify the existing tests still pass with `cargo test --test remote_cache`.
- [ ] 3.2 Send `parts` in the reserve body when `written.bytes >= part_bytes() * PARTED_MIN_PARTS` and the part count is at most 10,000. Parse `upload_parts` with the checks in design.md (Reserve and response parsing), and fall back to the single `PUT` when only `upload_url` is returned. Tests: a small archive's reserve has no `parts`; a `short_parts` reservation uploads nothing, sends no commit and warns; `single_url` saves with one `PUT`; with a 16-byte part override, an archive over 10,000 parts reserves without `parts`.
- [ ] 3.3 Upload the parts as in design.md (Uploading the parts): one upload client without a read timeout, at most `PART_CONNECTIONS` workers, `PART_ATTEMPTS` tries per part, each part streamed from its offset of the file with an explicit `Content-Length`, the join under `abortable`, then the commit with a 180-second bound instead of `REQUEST_TIMEOUT`. Tests: parts uploaded in parallel cover the archive exactly and the entry restores on another machine; no part request carries `Authorization`; with `flaky_parts = 1` the failed part is sent again and the save reports `cache_saved`; with `corrupt_part` that part is sent 3 times, no commit is sent, one `cache_warning` is printed and the local entry is kept.
- [ ] 3.4 Keep `422` bodies in `Remote::send` (same 1 MiB bound), and when a reserve that sent `parts` gets a `422` whose `errors` has a `parts` key, reserve once more without `parts`. Test: with `no_multipart`, goodflows sends two reserves (the second without `parts`), uploads one `PUT`, commits and reports `cache_saved`; any other `422` on reserve still warns "the cache reservation failed (HTTP 422)".
- [ ] 3.5 Bounds: a multipart upload that stalls (a `stall_put` variant for part `PUT`s) is abandoned at the cache's `timeout_secs` and keeps and reports the local entry, and a signal during a multipart upload names the cancellation, as the existing single-`PUT` tests do. Verify with `cargo test --test remote_cache`.
- [ ] 3.6 Restore verification: a test that saves an entry in parts, changes one byte of the stored object in the fake, and restores on another machine gets a `cache_warning`, stores nothing locally and runs the job as a miss. Verify with `cargo test --test remote_cache`.

## 4. Documentation

- [ ] 4.1 In README's cache section, say that archives of at least 64 MiB upload as parallel 16 MiB parts when the server supports it, fall back to one upload otherwise, and that restores always check size and sha256. Verify the paragraph matches the behavior in `specs/remote-cache/spec.md`.

## 5. Integration checks

- [ ] 5.1 Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test`, and verify all pass.
- [ ] 5.2 Benchmark against a local goodflows_web with the multipart change, backed by the R2 test bucket: save and restore 1 GB and 64 MiB archives with a release build, with and without `parts` (an older server, or the `no_multipart` path), and record the save and restore times in this change's design.md next to the 59.0 s / 19.6 s measurements. Verify the 1 GB multipart save is at least twice as fast as the single `PUT`, and that each restore reports `cache_restored` with `result` `exact`.
