# Proposal

## Why

A remote save uploads the whole archive as one presigned `PUT` over one connection, and that is slow. On the R2 test bucket, a 1 GB archive took 59.0 s as a single `PUT` (17 MB/s) and 19.6 s as 8 parallel 16 MiB multipart parts (51 MB/s). Large caches such as `_build/`, Cargo's `target/` or Dialyzer PLTs are the ones worth sharing, and they pay this cost on every save. Downloads already use parallel ranges (commit 0bd2701); uploads should be parallel too.

## What Changes

- For an archive of at least 64 MiB, the reserve request (`POST entries/:key`) also sends `parts`: `{"size": <part size>, "md5": [<base64 MD5 of each part>]}`. The archive is split into 16 MiB parts, which is at most 299 parts for the 5,000,000,000-byte cap.
- When the reservation returns `upload_parts`, goodflows `PUT`s each part to its own presigned URL with exactly that part's headers (`content-length`, `content-md5`) and no `Authorization` header. Up to 8 parts are in flight at once, and each part gets 3 tries. Then it commits as today. Smaller archives keep the single `PUT`.
- The part MD5s are computed while the archive is written, in the same pass as its sha256, so a large archive is not read again before the upload.
- Compatibility: if an older server answers the reserve with a `422` that names `parts`, goodflows retries once without `parts`, uploads with a single `PUT`, and reports the refusal with one `cache_warning` per run. If a server returns `upload_url` instead of `upload_parts`, goodflows uses the single `PUT`.
- A part that still fails after its tries fails the save the way a failed upload does today: one `cache_warning`, and the local entry is kept. The server aborts the abandoned multipart upload.
- Restores still verify size and sha256 before extracting. That check now also covers multipart entries, which the store cannot hash.
- No new events, config options or exit codes. This reverses the archived `add-job-caches` non-goal "Multipart uploads", because the measured speedup is worth it.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `remote-cache`: the upload requirement now covers multipart reservations. New requirements cover multipart part uploads, compatibility with older servers, and restore-time verification of multipart entries.

## Impact

- Code: `src/cache/remote.rs` (reserve body, `upload_parts` parsing, parallel part uploads, reading `422` bodies on reserve), `src/cache/archive.rs` (per-part MD5s in `Written`), `src/cache/mod.rs` (passing the part size to the writer when a remote store is enabled).
- Dependencies: `md-5` (RustCrypto, `0.10`, the same `digest` version as `sha2`) is new. `base64` (`0.22`) and `futures-util` (`0.3`, no default features) become direct dependencies; both are already in `Cargo.lock` through `reqwest`.
- API: uses the shared multipart contract that goodflows_web is adding at the same time (an optional `parts` on reserve, and `upload_parts` in the `201` response). The commit request is unchanged. Older servers keep working through the single-`PUT` fallback.
- Tests: the fake server in `tests/remote_cache.rs` models multipart uploads. README's cache section describes parallel uploads.
- Users: no configuration change. Large saves get faster against a server that supports `parts`.
