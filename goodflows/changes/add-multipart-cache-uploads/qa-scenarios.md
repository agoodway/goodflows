---
format: qa-scenarios/2
source: intent
change: add-multipart-cache-uploads
specs: uncommitted
scenarios:
  - id: 1
    name: Small archive uploads as one PUT
    priority: must
    covers:
      - {capability: remote-cache, requirement: Remote upload, scenario: Small archive}
  - id: 2
    name: Large archive is reserved with part MD5s
    priority: must
    covers:
      - {capability: remote-cache, requirement: Multipart reservation, scenario: 100 MiB archive}
      - {capability: remote-cache, requirement: Multipart reservation, scenario: Exactly 64 MiB}
  - id: 3
    name: Parts upload in parallel and restore elsewhere
    priority: must
    covers:
      - {capability: remote-cache, requirement: Multipart part uploads, scenario: Parts cover the archive}
      - {capability: remote-cache, requirement: Remote upload, implied: true}
  - id: 4
    name: A failing part is retried, then fails the save
    priority: must
    covers:
      - {capability: remote-cache, requirement: Multipart part uploads, scenario: Part fails once}
      - {capability: remote-cache, requirement: Multipart part uploads, scenario: Part rejected by its MD5}
  - id: 5
    name: A stalled multipart save stops at the cache bound
    priority: should
    covers:
      - {capability: remote-cache, requirement: Multipart part uploads, implied: true}
  - id: 6
    name: A reservation with a missing part uploads nothing
    priority: must
    covers:
      - {capability: remote-cache, requirement: Invalid multipart reservations, scenario: Wrong part count}
  - id: 7
    name: An older server falls back to one PUT and warns once
    priority: must
    covers:
      - {capability: remote-cache, requirement: Multipart compatibility, scenario: Older server}
  - id: 8
    name: A server without multipart storage gets one PUT
    priority: must
    covers:
      - {capability: remote-cache, requirement: Multipart compatibility, scenario: Server without multipart storage}
  - id: 9
    name: A tampered multipart entry is a miss
    priority: must
    covers:
      - {capability: remote-cache, requirement: Multipart entries are verified on restore, scenario: Tampered multipart entry}
---

# QA: add-multipart-cache-uploads

Setup for every scenario: a release build of goodflows, a workflow with one job that writes random (incompressible) data of the stated size into a cached path, `[cache] remote = "goodflows"` with a `[reporting]` block, a cache key in `GOODFLOWS_CACHE_KEY`, and a goodflows_web with the multipart change unless the scenario says otherwise. Run with `--json` and capture the server's request log.

## Scenarios

### 1. Small archive uploads as one PUT (Must)

1. Make the job write 10 MiB, and run on branch `main`.
2. Expected: the reserve body has no `parts`; there is exactly one `PUT`, to `upload_url`, with the returned `upload_headers`; a commit follows; `cache_saved` lists `local` and `remote`.

### 2. Large archive is reserved with part MD5s (Must)

1. Make the job write 100 MiB of random data and run.
2. Expected: the reserve body has `parts.size` 16777216 and `parts.md5` with 7 entries. Split the local archive file into 16 MiB slices; each entry equals the padded base64 MD5 of its slice, the last covering the final bytes.
3. Repeat with an archive of exactly 67108864 bytes (adjust the input until the stored archive is that size). Expected: 4 MD5s. With one byte fewer, the reserve has no `parts`.

### 3. Parts upload in parallel and restore elsewhere (Must)

1. Save a 100 MiB archive as in scenario 2.
2. Expected: 7 part `PUT`s, at most 8 in flight, each with exactly the part's returned headers and a `Content-Length` (no chunked body), none with `Authorization`; their bodies joined equal the archive; one commit; `cache_saved` lists `local` and `remote`.
3. On a second machine with an empty local store, run the same workflow. Expected: `cache_restored` with `result` `exact` from the remote tier.

### 4. A failing part is retried, then fails the save (Must)

1. Against a fake store, answer one part's first `PUT` with `503`. Expected: that part is sent again, no `cache_warning`, `cache_saved` lists `local` and `remote`.
2. Answer every try of one part with `400 BadDigest`. Expected: that part is sent 3 times with growing waits between tries; no commit; one `cache_warning` naming the failure; `cache_saved` lists only `local`; the local entry exists; the run's exit code is unchanged.

### 5. A stalled multipart save stops at the cache bound (Should)

1. Set the cache's `timeout_secs` to 5 and make the fake store stall part `PUT`s.
2. Expected: the save is abandoned at about 5 s with one `cache_warning` naming `timeout_secs`, no commit, and the local entry kept.
3. Run again and send SIGINT once part `PUT`s start. Expected: exit code 130 within a few seconds, one `cache_warning` naming the cancellation, no commit, and the local entry kept.

### 6. A reservation with a missing part uploads nothing (Must)

1. Make the server return one `upload_parts` entry fewer than requested for a 100 MiB save.
2. Expected: no `PUT` of any kind, no commit, one `cache_warning` about an invalid reservation, `cache_saved` lists only `local`.
3. Repeat with a part whose `content-md5` belongs to another part, a part URL on another host, and a part asking for `authorization`. Expected: the same result each time.

### 7. An older server falls back to one PUT and warns once (Must)

1. Use a server that answers a reserve with `parts` with `422` and `{"errors": {"parts": ["is not allowed"]}}`, and a workflow with two caches of at least 64 MiB.
2. Expected: four reserves, the first of each save with `parts` and the second without; one `PUT` per cache; two commits; both `cache_saved` events list `local` and `remote`; exactly one `cache_warning` in the run, naming the refused multipart upload.
3. Make the server answer `422` with another error key. Expected: one reserve, one `cache_warning` that the reservation failed with HTTP 422, `cache_saved` lists only `local`.

### 8. A server without multipart storage gets one PUT (Must)

1. Use a server that accepts `parts` but returns `upload_url` and `upload_headers`.
2. Expected: one reserve with `parts`, one `PUT` to `upload_url`, a commit, no `cache_warning`, `cache_saved` lists `local` and `remote`.

### 9. A tampered multipart entry is a miss (Must)

1. Save an entry in parts, then change one byte of the stored object in the bucket.
2. On another machine, run the same workflow.
3. Expected: one `cache_warning`, nothing stored locally from the remote, the job runs as a miss (`cache_restored` `result` `miss`), and the run passes.
