# Design

## Context

See proposal.md (Why) for the motivation and the measured speedup. The requirements are in `specs/remote-cache/spec.md`. Current state:

- `Remote::save` (`src/cache/remote.rs`) reserves, sends one `PUT` streamed from the file with `reqwest::Body::from(tokio::fs::File)` and an explicit `Content-Length`, then commits. The presigned client for uploads has no read timeout (commit bb977a3): reqwest's read timeout also covers the wait for response headers, which a store sends only after the whole body, so any upload slower than 30 s failed.
- `Remote::download` already fetches archives of at least `RANGED_MIN_PARTS` (4) × `PART_BYTES` (16 MiB) = 64 MiB as `Range` requests over `RANGED_CONNECTIONS` (8) workers in a `JoinSet` that pull part indexes from a shared counter, with `PART_ATTEMPTS` (3) tries per part. `part_bytes()` reads the debug-only `GOODFLOWS_TEST_CACHE_PART_BYTES` override. `abortable` makes the join end soon after the cache's abort flag is set, and dropping the `JoinSet` cancels the other parts.
- `archive::write` (`src/cache/archive.rs`) sends every compressed byte through `Counting`, a `Write` wrapper that counts, enforces the cap, checks the abort flag and feeds the sha256. `Written { bytes, sha256 }` comes from it. `src/cache/mod.rs` calls `archive::write` on a blocking thread, then hands the temp file and `Written` to the local store and to `Remote::save`.
- `Remote::send` keeps the response body only for 2xx statuses. A `422` body is dropped today.
- goodflows_web rejects unknown reserve body keys with `422` and `{"errors": {"<key>": ["is not allowed"]}}` (`ChangesetErrors.unknown_keys` in `GoodFlows.Caches`), so a server without this change refuses `parts` by name.

Facts verified against live R2, which constrain the approach:

- R2 answers `501 NotImplemented` to a part upload that carries `x-amz-checksum-sha256`, and a completed multipart object has no SHA-256. The store therefore cannot enforce the archive's sha256 on a multipart upload as it does on the single `PUT`.
- A presigned part URL signed with `Content-MD5` and `Content-Length` rejects a body that does not match (`400 BadDigest`) and a request without the header (`403`). Per-part MD5 is therefore the integrity check the store can enforce.
- Parts are 5 MiB to 5 GiB, all but the last the same size, at most 10,000.
- 1 GB on the R2 test bucket: 59.0 s as one `PUT` (17 MB/s), 19.6 s as 8 parallel 16 MiB parts (51 MB/s).

## Goals / Non-Goals

**Goals:**
- Parallel uploads for large archives, with the same memory profile as today (streamed from disk, never whole in memory) and the same time bound (`timeout_secs`, cancellation).
- One write pass over the archive: the part MD5s are ready when `archive::write` returns.
- Keep working against servers and stores that do not support `parts`.

**Non-Goals:**
- Resuming a failed save across runs, or reusing parts from an earlier attempt. A failed save is abandoned; the next run uploads again.
- Aborting the multipart upload from the CLI. The server owns the upload ID and aborts expired ones.
- A configurable part size or concurrency. The constants match the download path and the R2 measurement.
- Changing the download path or the commit request.

## Decisions

### When to use parts, and their size

Send `parts` when `written.bytes >= part_bytes() * RANGED_MIN_PARTS` (64 MiB), with part size `part_bytes()` (16 MiB). This uses the same threshold and size as ranged downloads, so the two paths stay symmetric and share the debug override. Below 64 MiB, the extra round trips and the reservation of up to 4 parts buy little over one connection, which is the same reasoning as for downloads.

The 10,000-part limit cannot be reached in a release build: `MAX_ENTRY_BYTES` / 16 MiB = 5,000,000,000 / 16,777,216 ≈ 298.0, so at most 299 parts. The brief suggested raising the part size when needed. That would not work with MD5s computed while writing, because the part boundaries must be fixed before the archive's size is known, and MD5s of 16 MiB slices cannot be combined into the MD5 of a larger slice. Only the debug override (for example 16-byte parts) can produce more than 10,000 parts, and then the save uses the single `PUT`. A test pins this. *Alternative:* a second read pass with a larger part size, which is extra code for a case that release builds never reach.

The constants are renamed to path-neutral names (`PART_BYTES`, `PARTED_MIN_PARTS`, `PART_CONNECTIONS`, `PART_ATTEMPTS`), and their doc comments name both directions.

### Part MD5s while writing

This is feasible in `archive.rs` with no second pass. `Counting::write` already sees every compressed byte in order and knows the running `count`. Add an optional per-part MD5 state: the part size, the current `md5::Md5` hasher, and the finished digests. For each written slice, feed the hasher up to the next part boundary, finish it and start a new one, and continue with the rest of the slice. The last, short part is finished when the writer finishes. `Written` gains `part_md5: Option<(u64, Vec<String>)>` (the part size and the base64 digests).

`archive::write` takes `parts: Option<u64>`. `src/cache/mod.rs` passes `Some(part_bytes())` only when a remote store is configured and enabled, so local-only runs do no MD5 work. The writer does not know the final size, so it computes MD5s for every archive when parts are requested. That costs little: MD5 runs at roughly 600+ MB/s on one core, far faster than gzip at the default level, which is the writer's bottleneck. Small archives ignore the result.

New dependency: `md-5 = "0.10"` (RustCrypto; its library name is `md5`, and it uses the same `digest` 0.10 traits as `sha2`). Add `[profile.dev.package.md-5] opt-level = 3` next to the `sha2` entry, so debug test runs stay fast. Base64 uses `base64 = "0.22"`, already in `Cargo.lock` through reqwest, as a direct dependency (standard alphabet, with padding, as `Content-MD5` requires). *Alternative:* the `md5` crate, which works but is not RustCrypto and does not share `digest` with `sha2`.

### Reserve and response parsing

The reserve body adds `"parts": {"size": S, "md5": [...]}` when the archive qualifies. The `201` is parsed as follows:

- `upload_parts` present: it must be an array of exactly ceil(size / S) objects, where object `i` has `number == i + 1`, a `url` accepted by `presigned()` with the same host and scheme as part 1, and a `headers` object of string values. `content-length` must equal that part's length, and `content-md5` must equal the digest goodflows sent. A header named `authorization` makes the reservation invalid, as today. Any other headers are sent as given. A mismatch fails the save with "the cache server returned an invalid reservation" before any byte is uploaded. Pinning all URLs to part 1's host keeps a single client (built for part 1's URL, with loopback `no_proxy`) correct for every part, and checking the headers catches a server that mixed up parts before the store would reject them one by one.
- No `upload_parts`, but `upload_url` and `upload_headers` present: take the single-`PUT` path, which is unchanged.
- Neither: invalid reservation, as today.

`upload_id`, `upload_url_expires_at` and `expires_at` are read as today. The part URLs stay valid for the whole reservation (about 2 hours), which is longer than any `timeout_secs` bound, so there is no refresh logic.

### Compatibility with older servers

`Remote::send` also keeps the body of a `422` (same 1 MiB bound). When a reserve that sent `parts` gets `422` and `errors` has a `parts` key, `save` sends the same reserve again once without `parts` and continues with whatever that returns. Any other `422` is handled as today ("the cache reservation failed (HTTP 422)"). A new server that rejects `parts` for its own reasons, such as a malformed MD5, also lands on the single `PUT`. That is acceptable, because the single `PUT` works for every archive up to the cap. *Alternative:* a capability probe or a version header, which costs a request on every save and requires a server change that the contract does not include.

### Uploading the parts

The part uploader has the same shape as `download_parts`: one `presigned_client(&first_url, true)` (no read timeout, for the bb977a3 reason, which applies to each 16 MiB part on a slow link), `min(parts, PART_CONNECTIONS)` workers in a `JoinSet` that pull indexes from an `AtomicU64`, and up to `PART_ATTEMPTS` tries per part on any error (transport error, non-2xx status). On each try, a worker opens the archive (`tokio::fs::File`), seeks to `index * S`, wraps `file.take(len)` in `tokio_util::io::ReaderStream` and sends it with `reqwest::Body::wrap_stream`, the part's headers and an explicit `Content-Length`, so the body is never chunked. Each part is read from disk as it is sent. Memory use is a few stream buffers per worker, never a part or the archive. `tokio-util` (`0.7`, feature `io`) becomes a direct dependency; it is already in `Cargo.lock`.

Every error is retried, which mirrors downloads. A `400 BadDigest` from in-flight corruption can succeed on a retry. A deterministic rejection (an expired URL, or a file that changed on disk) costs at most two more part sends before the save fails. Before the workers start, `save` checks once that the file's length still equals `written.bytes`, as the single `PUT` does now.

The join is wrapped in `abortable(abort, …)`. When `timeout_secs` passes or the run is cancelled, the `JoinSet` is dropped, which cancels every in-flight part, and `save` returns `Saved::Unavailable`. `mod.rs` then reports the abandoned upload as it does today. A part that fails after its tries returns `Saved::Failed("the cache upload failed (HTTP n)" / "…could not reach the object store")`, which becomes one `cache_warning`. No commit is sent, and the local entry stays, because the local store wrote it before the remote save. The server aborts the incomplete multipart upload when the reservation expires.

### Commit and restore

The commit request is unchanged (`POST uploads/:upload_id/commit`, no body), except that a multipart commit is given 180 seconds instead of the 30-second `REQUEST_TIMEOUT`: goodflows_web's design (`add-multipart-cache-uploads`, M6) allows its CompleteMultipartUpload call 120 seconds, so a 30-second client bound would abandon commits the server is still finishing. The cache's `timeout_secs` and the abort flag still bound it. The server lists the parts, checks them, completes the upload and checks the result, and `422` still maps to "the stored object does not match". Restores already check size and sha256 before extracting (`stream_whole` for small entries, `archive::extract` after ranged downloads), so multipart entries are covered without a code change. The new requirement makes that an explicit contract, because the store no longer guarantees the sha256.

### Tests (fake server in `tests/remote_cache.rs`)

The fake models multipart uploads:
- On reserve with `parts`, it checks the count against `size`, stores the MD5 list and returns `upload_parts` with `/objects/<id>/parts/<n>?X-Amz-Signature=sig` URLs and `content-length`/`content-md5` headers.
- A part `PUT` must carry exactly those headers (`403` when one is missing) and a body whose length and MD5 match (`400` otherwise).
- On commit, it requires every part, joins them, and checks the joined size and sha256 against the reservation (`422` otherwise). For a single `PUT` it behaves as today.

New flags:
- `no_multipart`: reject `parts` with the goodflows_web `422` body.
- `single_url`: ignore `parts` and return `upload_url`.
- `flaky_parts: usize`: fail that many part `PUT`s with `503`.
- `corrupt_part: Option<usize>`: flip one received byte of that part before the MD5 check, so it gets `400 BadDigest` on every try.
- `short_parts`: return one part fewer than requested.

Scenarios use `SMALL_PARTS`-style overrides so archives of a few KiB use parts.

## Risks / Trade-offs

- [The store cannot hash a multipart object, so a bad completion would be stored as committed] → part MD5s are enforced by the store's signature, the server checks the part list and sizes at commit, and every restore checks size and sha256 before extracting. A bad entry costs one wasted download and counts as a miss, never a bad extraction.
- [8 parallel `PUT`s may saturate a slow uplink, so a single part takes longer] → a part has no read timeout, and the whole save is bounded by `timeout_secs`. This is the same trade-off ranged downloads already make.
- [MD5 work on every archive when a remote store is enabled, including small ones] → MD5 is several times faster than the gzip that produces the bytes. Skipping it for small archives would need the final size up front.
- [An older server's `422` costs one extra reserve request] → only for archives of at least 64 MiB, and only once per save.
- [Abandoned multipart uploads hold storage until the server aborts them] → this is the server's job (goodflows_web change); reservations expire in about 2 hours.
- [The debug override can produce more than 10,000 parts] → the save uses the single `PUT` instead; release builds cannot reach the limit (at most 299 parts).

## Migration Plan

No user action. A CLI with this change against an older server falls back to the single `PUT` after one `422`. An older CLI against a new server never sends `parts` and keeps getting `upload_url`. Rollback is reverting the CLI; entries already saved in parts restore like any other entry.

## Open Questions

- Once the server change is deployed, does R2 or the server keep a part-count or part-size limit stricter than S3's? The benchmark task (tasks.md, group 5) will show it. The 16 MiB part size is well inside the published limits.
