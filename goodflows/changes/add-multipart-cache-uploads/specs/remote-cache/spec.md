# Spec Delta

## MODIFIED Requirements

### Requirement: Remote upload
Refs: G1, G4, D1
To save remotely, goodflows SHALL reserve the key with `POST entries/:key` (body: name, partial, scope, size, sha256, and `parts` for a multipart upload), upload the archive, and commit with `POST uploads/:upload_id/commit`. A reservation with `upload_url` and no `upload_parts` SHALL be uploaded as one `PUT` to that URL with exactly the returned `upload_headers`. A `409` on reserve SHALL count as saved. A failed upload MUST NOT remove the local entry.

#### Scenario: Two partitions save the same key
- **WHEN** two partition instances save the same key and the second reserve returns `409`
- **THEN** both report `cache_saved`, and only one archive is uploaded

#### Scenario: Small archive
- **WHEN** a 10 MiB archive is saved
- **THEN** the reserve body has no `parts`, and the archive is uploaded as one `PUT` to `upload_url`

## ADDED Requirements

### Requirement: Multipart reservation
Refs: G1, G3, D1, D2
For an archive of at least 64 MiB, the reserve body SHALL include `parts`: `{"size": S, "md5": [...]}`, where S is 16777216 (16 MiB) and `md5` lists, in order, the standard padded base64 MD5 of each S-byte slice of the archive file, the last slice shorter. There SHALL be ceil(size / S) entries. A smaller archive, or one that would need more than 10,000 parts, SHALL be reserved without `parts`. The part MD5s SHALL be computed while the archive is written, so the archive is not read again before its upload.

#### Scenario: 100 MiB archive
- **WHEN** a 100 MiB archive is saved
- **THEN** the reserve body has `parts.size` 16777216 and seven MD5s, the last one covering the final 4 MiB

#### Scenario: Exactly 64 MiB
- **WHEN** a 67108864-byte archive is saved
- **THEN** the reserve body has `parts` with four MD5s

### Requirement: Multipart part uploads
Refs: G1, G2, D5, D6, A2, A3
When a reservation returns `upload_parts`, goodflows SHALL `PUT` each part's bytes to its `url` with exactly its `headers` and a matching `Content-Length` (never a chunked body), and no `Authorization` header, with at most 8 parts in flight, streaming each part from the archive on disk. A part SHALL be tried up to 3 times on any error, waiting 250 ms times the number of failed tries before the next. A part that still fails SHALL fail the save with one `cache_warning`, without a commit; `cache_saved` then lists only the `local` tier, and the local entry SHALL be kept. After every part succeeds, goodflows SHALL commit as for one `PUT`, allowing the commit up to 180 seconds. The cache's `timeout_secs` and run cancellation SHALL bound the whole multipart save as they bound one `PUT`, stopping every in-flight part and keeping the local entry.

#### Scenario: Parts cover the archive
- **WHEN** a 100 MiB archive is uploaded in parts
- **THEN** the seven part bodies together equal the archive byte for byte, no part request carries `Authorization`, and the entry is committed

#### Scenario: Part fails once
- **WHEN** the store answers one part's first `PUT` with `503`
- **THEN** goodflows sends that part again, and `cache_saved` lists the `local` and `remote` tiers

#### Scenario: Part rejected by its MD5
- **WHEN** the store answers every try of one part with `400 BadDigest`
- **THEN** that part is sent 3 times, no commit is sent, goodflows reports one `cache_warning`, and the local entry is kept

### Requirement: Invalid multipart reservations
Refs: D3, A2
goodflows SHALL treat a reservation as invalid, and fail the save with a `cache_warning` before uploading anything, when `upload_parts` is not a list of each requested part once in order (`number` 1 to n), a part's `content-length` or `content-md5` differs from that part, a part URL is not an `https` URL (or `http` on loopback) on the first part's host, port and scheme, a part asks for an `Authorization` header, or `upload_parts` answers a reserve that sent no `parts`.

#### Scenario: Wrong part count
- **WHEN** a reservation for seven parts returns six `upload_parts`
- **THEN** no part is uploaded, no commit is sent, and goodflows reports a `cache_warning`

### Requirement: Multipart compatibility
Refs: G4, D4
When a reserve that sent `parts` gets a `422` whose `errors` has a `parts` key, goodflows SHALL reserve once more without `parts` and upload with one `PUT`, and SHALL report the refusal with one `cache_warning` per run, however many saves fall back; each later save in the run still tries `parts` first. Any other `422` on reserve SHALL fail the save with a `cache_warning`, without a second reserve. When a reservation that was sent `parts` returns `upload_url` and no `upload_parts`, goodflows SHALL upload with one `PUT` and no warning.

#### Scenario: Older server
- **WHEN** the server answers a reserve with `parts` with `422` and `{"errors": {"parts": ["is not allowed"]}}`
- **THEN** goodflows reserves again without `parts`, uploads one `PUT`, commits, and `cache_saved` lists the `local` and `remote` tiers, with one `cache_warning` naming the refused multipart upload

#### Scenario: Server without multipart storage
- **WHEN** a reserve with `parts` returns `upload_url` and `upload_headers`
- **THEN** the archive is uploaded as one `PUT` to `upload_url`

### Requirement: Multipart entries are verified on restore
Refs: G5, D6
Because the store does not check a multipart object's sha256, goodflows SHALL verify every downloaded entry against the size and sha256 in its metadata before extracting, however it was uploaded.

#### Scenario: Tampered multipart entry
- **WHEN** an entry saved in parts is changed in the bucket after its commit
- **THEN** a restore discards it, reports a `cache_warning`, stores nothing locally, and treats the cache as a miss
