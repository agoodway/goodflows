# Spec Delta

## MODIFIED Requirements

### Requirement: Remote upload
To save remotely, goodflows SHALL reserve the key with `POST entries/:key` (body: name, partial, scope, size, sha256, and `parts` for a multipart upload), upload the archive, and commit with `POST uploads/:upload_id/commit`. A reservation with `upload_url` SHALL be uploaded as one `PUT` to that URL with exactly the returned `upload_headers`. A `409` on reserve SHALL count as saved. A failed upload MUST NOT remove the local entry.

#### Scenario: Two partitions save the same key
- **WHEN** two partition instances save the same key and the second reserve returns `409`
- **THEN** both report `cache_saved`, and only one archive is uploaded

#### Scenario: Small archive
- **WHEN** a 10 MiB archive is saved
- **THEN** the reserve body has no `parts`, and the archive is uploaded as one `PUT` to `upload_url`

## ADDED Requirements

### Requirement: Multipart reservation
For an archive of at least 64 MiB, the reserve body SHALL include `parts`: `{"size": S, "md5": [...]}`, where S is 16 MiB and `md5` lists the base64 MD5 of each S-byte slice of the archive in order, the last one shorter. There SHALL be ceil(size / S) entries, at most 10,000. A smaller archive SHALL be reserved without `parts`.

#### Scenario: 100 MiB archive
- **WHEN** a 100 MiB archive is saved
- **THEN** the reserve body has `parts.size` 16777216 and seven MD5s, the last one covering the final 4 MiB

### Requirement: Multipart part uploads
When a reservation returns `upload_parts`, goodflows SHALL `PUT` each part's bytes to its `url` with exactly its `headers` and no `Authorization` header, with at most 8 parts in flight, then commit. A part SHALL be tried up to 3 times. A part that still fails SHALL fail the save with a `cache_warning`, without a commit, and the local entry SHALL be kept.

#### Scenario: Parts cover the archive
- **WHEN** a 100 MiB archive is uploaded in parts
- **THEN** the seven part bodies together equal the archive byte for byte, no part request carries `Authorization`, and the entry is committed

#### Scenario: Part fails once
- **WHEN** the store answers one part's first `PUT` with `503`
- **THEN** goodflows sends that part again, and the save reports `cache_saved`

#### Scenario: Part rejected by its MD5
- **WHEN** the store answers every try of one part with `400 BadDigest`
- **THEN** that part is sent 3 times, no commit is sent, goodflows reports a `cache_warning`, and the local entry is kept

### Requirement: Invalid multipart reservations
goodflows SHALL treat a reservation as invalid, and fail the save with a `cache_warning` before uploading anything, when `upload_parts` does not list each requested part once in order, a part's `content-length` or `content-md5` differs from that part, a part URL is not a presigned URL on the first part's host and scheme, or a part asks for an `Authorization` header.

#### Scenario: Wrong part count
- **WHEN** a reservation for seven parts returns six `upload_parts`
- **THEN** no part is uploaded, no commit is sent, and goodflows reports a `cache_warning`

### Requirement: Multipart compatibility
When a reserve that sent `parts` gets a `422` whose errors name `parts`, goodflows SHALL reserve once more without `parts` and upload with one `PUT`. When a reservation that was sent `parts` returns `upload_url` and no `upload_parts`, goodflows SHALL upload with one `PUT`.

#### Scenario: Older server
- **WHEN** the server answers a reserve with `parts` with `422` and `{"errors": {"parts": ["is not allowed"]}}`
- **THEN** goodflows reserves again without `parts`, uploads one `PUT`, commits, and reports `cache_saved`

#### Scenario: Server without multipart storage
- **WHEN** a reserve with `parts` returns `upload_url` and `upload_headers`
- **THEN** the archive is uploaded as one `PUT` to `upload_url`

### Requirement: Multipart entries are verified on restore
Because the store does not check a multipart object's sha256, goodflows SHALL verify every downloaded entry against the size and sha256 in its metadata before extracting, however it was uploaded.

#### Scenario: Tampered multipart entry
- **WHEN** an entry saved in parts is changed in the bucket after its commit
- **THEN** a restore discards it, reports a `cache_warning`, stores nothing locally, and treats the cache as a miss
