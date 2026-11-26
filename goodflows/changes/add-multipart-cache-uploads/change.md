---
format: goodflows-change/1
id: add-multipart-cache-uploads
status: approved
size: small
door: two_way
capabilities: [remote-cache]
depends_on: []
goals:
  - id: G1
    text: "Large archives upload as parallel parts when the server supports it."
  - id: G2
    text: "Parallel uploads keep today's memory profile (streamed from disk, never whole in memory) and time bound (the cache's timeout_secs and cancellation)."
  - id: G3
    text: "One write pass: the part MD5s are ready when the archive is written, so a large archive is not read again before its upload."
  - id: G4
    text: "Saves keep working against servers and stores that do not support parts."
  - id: G5
    text: "Restores verify multipart entries, which the store cannot hash."
non_goals:
  - id: N1
    text: "Resuming a failed save across runs, or reusing parts from an earlier attempt. A failed save is abandoned and the next run uploads again."
  - id: N2
    text: "Aborting the multipart upload from the CLI. The server owns the upload ID and aborts expired ones."
  - id: N3
    text: "A configurable part size or concurrency."
  - id: N4
    text: "Changing the download path or the commit request."
anti_goals:
  - id: A1
    text: "New events, config options or exit codes."
  - id: A2
    text: "Sending an Authorization header or the cache key to the object store."
  - id: A3
    text: "Holding a whole part or the whole archive in memory."
decisions:
  - id: D1
    question: "When does a save use parts, and how big are they?"
    answer: "Archives of at least 4 parts (64 MiB) are reserved with parts of 16 MiB, the same threshold, size and debug override as ranged downloads. An archive that would need more than 10,000 parts, which only the debug override can produce, is reserved without parts."
    by: agent
    why: "Keeps uploads and downloads symmetric; below 64 MiB the extra round trips buy little over one connection. In a release build the 5,000,000,000-byte cap is at most 299 parts, so the 10,000-part limit is never reached. Part boundaries must be fixed before the archive's size is known, because MD5s of 16 MiB slices cannot be combined into the MD5 of a larger slice."
    rejected:
      - "Raising the part size for very large archives: incompatible with MD5s computed while writing."
      - "A second read pass with a larger part size: extra code for a case release builds never reach."
  - id: D2
    question: "When and how are the part MD5s computed?"
    answer: "In the archive writer, in the same pass as the sha256, splitting each written slice at part boundaries. They are computed for every archive whenever a remote store is enabled and ignored for small archives; local-only runs do no MD5 work."
    by: agent
    why: "The writer sees every compressed byte in order but not the final size. MD5 runs at 600+ MB/s on one core, far faster than gzip, the writer's bottleneck."
    rejected:
      - "Skipping MD5 for small archives: needs the final size up front."
      - "The `md5` crate: works, but is not RustCrypto and does not share `digest` with `sha2`."
  - id: D3
    question: "How strictly is a multipart reservation checked?"
    answer: "Fully, before any byte is uploaded: each requested part exactly once, in order, on the first part's host, port and scheme, with that part's own content-length and content-md5, and no authorization header. Other headers are sent as given. Part URLs are not refreshed."
    by: agent
    why: "Pinning every URL to part 1's host lets one presigned client serve all parts, and checking headers catches a server that mixed up parts before the store rejects them one by one. Part URLs stay valid for the whole reservation (about 2 hours), longer than any timeout_secs bound."
  - id: D4
    question: "How does a new CLI work with a server that does not know `parts`?"
    answer: "A reserve with `parts` that gets a 422 whose errors name `parts` is sent once more without `parts`, and the save uses one PUT. The first fallback in a run prints one cache_warning. A reservation that returns `upload_url` instead of `upload_parts` also uses one PUT."
    by: agent
    why: "goodflows_web rejects unknown reserve keys by name, so an older server identifies itself at the cost of one extra reserve, and only for archives of at least 64 MiB. The warning makes a server that refuses `parts` for its own reasons visible rather than silently slower; the single PUT works for every archive up to the cap."
    rejected:
      - "A capability probe or version header: a request on every save, and a server change the contract does not include."
  - id: D5
    question: "How are the parts sent?"
    answer: "Up to 8 parts in flight, workers pulling the next part index; each part tried up to 3 times on any error, waiting 250 ms times the number of failed tries before the next; each try streamed afresh from the part's offset in the already-open archive with positioned reads and an explicit Content-Length; no read timeout on part requests. A part that fails all tries fails the save; the timeout or a cancellation drops every in-flight part."
    by: agent
    why: "Mirrors ranged downloads. A store sends response headers only after the whole body, so a read timeout would fail slow parts (as commit bb977a3 found for the single PUT). The backoff keeps retries out of one 503 SlowDown window. A 400 BadDigest from in-flight corruption can succeed on a retry; a deterministic rejection costs at most two more sends. Positioned reads on one handle mean concurrent parts and retries share no file offset, and nothing reopens the staged archive by path, which local store pruning may remove after an hour."
  - id: D6
    question: "What changes about commit and restore?"
    answer: "The commit request is unchanged, but a multipart commit is allowed 180 seconds instead of 30. Restore keeps its existing size and sha256 check before extraction, now stated as a contract for multipart entries."
    by: agent
    why: "goodflows_web allows its CompleteMultipartUpload call 120 seconds, so a 30-second client bound would abandon commits the server is still finishing; timeout_secs and cancellation still bound it. The store no longer guarantees the sha256, so the restore check is the only end-to-end integrity check."
left_to_builder:
  - "Names and placement of the shared part constants, the MD5 state in the archive writer, the reservation parser and the part uploader. The upload constants are shared with ranged downloads, so the two paths cannot drift apart."
  - "Wording of the cache_warning messages. The fallback warning must name the refused multipart upload and be printed once per run; a failed part's warning should carry the HTTP status or say the object store could not be reached; another 422 on reserve keeps today's reservation-failed warning."
  - "Dependencies: an MD5 implementation that shares the `digest` traits with `sha2` (md-5 0.10 was chosen, optimized in dev builds like sha2), base64 with the standard padded alphabet that Content-MD5 requires, and a stream helper for part bodies."
  - "The chunk size of part reads (256 KiB was chosen), as long as memory stays at a few chunks per worker."
  - "How the fake server in tests/remote_cache.rs models multipart reserves, part PUTs and commits, and which failure flags it offers."
  - "The README cache section's description of parallel uploads, which must match the remote-cache spec."
---

## Why

A remote save uploads the whole archive as one presigned `PUT` over one connection, which is slow. Large caches such as `_build/`, Cargo's `target/` or Dialyzer PLTs are the ones worth sharing, and they pay that cost on every save. Downloads already use parallel ranges (commit 0bd2701); uploads should be parallel too. This reverses the archived `add-job-caches` non-goal "Multipart uploads", because the measured speedup is worth it.

## Context

Measured on the R2 test bucket: a 1 GB archive took 59.0 s as one `PUT` (17 MB/s) and 19.6 s as 8 parallel 16 MiB parts (51 MB/s).

Server contract, shared with goodflows_web's `add-multipart-cache-uploads` change: the reserve (`POST entries/:key`) takes an optional `parts: {"size": S, "md5": [...]}`, and its `201` then carries `upload_parts`, a list of `{number, url, headers}`, instead of `upload_url` and `upload_headers`. The commit request is unchanged. A server without the change rejects unknown reserve keys with `422` and `{"errors": {"parts": ["is not allowed"]}}`. The server aborts abandoned multipart uploads when the reservation expires, after about 2 hours, and allows itself 120 seconds to complete one during the commit.

Facts verified against live R2:

- R2 answers `501 NotImplemented` to a part upload carrying `x-amz-checksum-sha256`, and a completed multipart object has no SHA-256, so the store cannot enforce the archive's sha256 as it does on a single `PUT`.
- A part URL presigned with `Content-MD5` and `Content-Length` rejects a body that does not match (`400 BadDigest`) and a request without the header (`403`). Per-part MD5 is the integrity check the store can enforce.
- Parts are 5 MiB to 5 GiB, all but the last the same size, and at most 10,000.

Existing behavior this change builds on: ranged downloads already use 16 MiB parts from 64 MiB up, 8 connections, 3 tries per part, and a debug-only part-size override. Presigned uploads have no read timeout (commit bb977a3). The archive writer already counts, caps and hashes every compressed byte. Restores already check size and sha256 before extracting.

Rollout needs no user action. An older CLI never sends `parts` and keeps getting `upload_url`. Rolling back means reverting the CLI; entries saved in parts restore like any other.

## Approaches

Parts are sized and their MD5s computed at write time (D1, D2), because the alternative, a second read pass that could choose a larger part size, costs a full re-read of archives up to 5 GB and only helps a case release builds cannot reach. Compatibility is negotiated in-band through the server's existing `422` for unknown keys (D4) rather than a capability probe. The uploader copies the ranged-download design (D5) so both directions share constants, retry behavior and cancellation. Since the store cannot verify a multipart object's sha256, integrity rests on three layers: per-part MD5 enforced by the presigned signature, the server's part check at commit, and the restore-time size and sha256 check (D6). A bad entry therefore costs one wasted download and a miss, never a bad extraction.

Accepted trade-offs: 8 parallel `PUT`s can saturate a slow uplink so one part takes longer, bounded by `timeout_secs` as ranged downloads already are. Abandoned multipart uploads hold storage until the server aborts them.

## Still to verify

The code is merged (commit fe24bfa). The benchmark is still open: save and restore 1 GB and 64 MiB archives with a release build against goodflows_web with the multipart change, backed by the R2 test bucket, with and without `parts`. The pass criteria are that a 1 GB save in parts is at least twice as fast as the single `PUT`, and that each restore reports `cache_restored` with `result` `exact`.

A run on 2026-11-19 (random data, so gzip could not shrink it; times are `duration_ms` of `cache_saved` and `cache_restored`; "single PUT" is the CLI at bb977a3 against the same server) did not settle it:

| Archive | Archive build (local-only save) | Save, single PUT | Save, parts | Upload only, single PUT → parts |
| --- | --- | --- | --- | --- |
| 1 GB (60 parts) | 34.3 s | 61.1, 64.2, 65.1 s (median 64.2) | 46.8, 49.1, 58.9 s (median 49.1) | ≈ 29.9 s → ≈ 14.8 s (2.0×) |
| 70 MB (5 parts) | 2.6 s | 7.3, 9.2, 10.8 s (median 9.2) | 5.3, 5.2, 5.6 s (median 5.3) | ≈ 6.6 s → ≈ 2.7 s (2.4×) |

The upload alone is about twice as fast, matching the raw R2 measurement, but the whole 1 GB save is only about 1.3× faster, because single-threaded gzip (34 s for 1 GB) now dominates. The criterion as worded is not met. Restores of 1 GB entries saved in parts reported `exact` in 5.8–6.3 s, against 6.2–13.3 s for a single-PUT entry of the same size. Two earlier 1 GB restores took about 20 s while R2 was slow for both kinds, and 70 MB restored in 3.8 and 1.1 s. A rerun after the review fixes (256 KiB part reads, retry backoff) with R2 noisier gave a 33.9 s build, 1 GB saves of 55.7 and 59.2 s single against 46.1 and 54.5 s in parts, and 1 GB restores of 41.9, 8.4, 8.2, 8.0 and 5.9 s, all `exact` with no warnings. It cannot separate the read-size change from R2's variance.

Still needed: a run against the deployed server, at the 64 MiB boundary that the task names (70 MB was used), a decision on whether the 2× criterion applies to the whole save or to the upload, and confirmation that neither R2 nor the server enforces a part-count or part-size limit stricter than S3's.
