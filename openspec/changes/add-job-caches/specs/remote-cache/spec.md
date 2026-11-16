# Spec Delta

## Purpose

Defines the remote cache store on a GoodFlows server: the credentials goodflows uses, how it looks entries up and uploads them, how branch scopes and server errors affect a run, and how Sprite runs and long-lived environments get cache keys. The server side is specified by goodflows_web's `add-remote-cache-storage` change.

## ADDED Requirements

### Requirement: Remote store configuration
`[cache] remote = "goodflows"` SHALL enable the remote store. It MUST have a `[reporting]` block, and it SHALL use that block's `project` and `url_env`.

#### Scenario: Remote without reporting
- **WHEN** `[cache] remote = "goodflows"` is set and there is no `[reporting]` block
- **THEN** validation fails before any job runs

### Requirement: Cache key source
goodflows SHALL read the cache key from the variable named by `[cache] token_env` (default `GOODFLOWS_CACHE_KEY`), and SHALL use the `[reporting]` key when that variable is unset or empty. `token_env` MUST follow the reporting rules for credential variable names, and MAY name the reporting `token_env`.

#### Scenario: Separate cache key
- **WHEN** `GOODFLOWS_CACHE_KEY` holds a `ck_` key and `GOODFLOWS_API_KEY` holds an `sk_` key
- **THEN** cache requests use the `ck_` key, and run-history and report uploads use the `sk_` key

### Requirement: Missing credentials
When no cache key or URL is available when the run starts, goodflows SHALL use only the local store and print one `cache_remote_unavailable` event with reason `credentials_unavailable`.

#### Scenario: Laptop without keys
- **WHEN** neither `GOODFLOWS_CACHE_KEY` nor the reporting key is set
- **THEN** the run uses the local store, prints one `cache_remote_unavailable` event, and makes no cache request

### Requirement: Cache scope
goodflows SHALL take the remote scope from `[cache] scope`, an expression that MAY read only `env.*`. When it is unset, evaluates to null or to an empty string, or yields a string that breaks the server's scope rule (over 256 UTF-8 bytes, or with control characters), goodflows SHALL use the run's Git branch, with one `cache_warning` for an invalid result. Lookups SHALL send the scope when there is one and omit it otherwise.

#### Scenario: Pull request on a detached checkout
- **WHEN** `scope = "env.GITHUB_HEAD_REF ?? env.GITHUB_REF_NAME"`, the checkout is detached, and `GITHUB_HEAD_REF` is `feat-x`
- **THEN** lookups and saves use scope `feat-x`

#### Scenario: Scope reads a job output
- **WHEN** `[cache] scope` reads `needs.build.outputs.branch`
- **THEN** validation fails before any job runs

### Requirement: Read keys bound to another scope
When a lookup that sends a scope gets `403`, goodflows SHALL send every later lookup in that run without a scope, so the server reads only the default branch, and SHALL print one `cache_warning` for the run.

#### Scenario: Orb on a feature branch
- **WHEN** a run on branch `feature/login` uses a read key bound to `main`, and its first lookup with scope `feature/login` gets `403`
- **THEN** goodflows repeats that lookup without a scope, restores main's entry when there is one, and prints one `cache_warning`

### Requirement: Runs without a scope
A run with no scope MUST NOT save to the remote store, and SHALL report `cache_save_skipped` with reason `no_scope`. goodflows MUST NOT add scopes of its own; the server decides the lookup order.

#### Scenario: Detached checkout without a scope setting
- **WHEN** a run on a detached HEAD with no `[cache] scope` restores and then passes after a miss
- **THEN** lookups carry no scope, the entry is saved locally only, and the remote save is skipped with reason `no_scope`

### Requirement: Remote lookup and download
goodflows SHALL look up an exact key with `GET entries/:key` and a fallback with `GET entries?partial=P`, both with the run's scope. It SHALL download from the returned `download_url` without an `Authorization` header and without following redirects to other hosts. Before extracting, it MUST check that the downloaded size and sha256 match the metadata. A `404` is a miss.

#### Scenario: Tampered download
- **WHEN** a downloaded archive's sha256 differs from the metadata
- **THEN** goodflows discards it, reports a `cache_warning`, stores nothing locally, and treats the cache as a miss

#### Scenario: Presigned URL
- **WHEN** goodflows downloads or uploads through a presigned URL
- **THEN** the request carries no `Authorization` header and no cache key

### Requirement: Remote upload
To save remotely, goodflows SHALL reserve the key with `POST entries/:key` (body: name, partial, scope, size, sha256), `PUT` the archive to `upload_url` with exactly the returned `upload_headers`, and commit with `POST uploads/:upload_id/commit`. A `409` on reserve SHALL count as saved. A failed upload MUST NOT remove the local entry.

#### Scenario: Two partitions save the same key
- **WHEN** two partition instances save the same key and the second reserve returns `409`
- **THEN** both report `cache_saved`, and only one archive is uploaded

### Requirement: Upload refusals
goodflows SHALL map upload refusals as follows. A `403` on reserve makes the run read-only, with one `cache_warning` for the run and `cache_save_skipped` reason `read_only` for each later save. A `413` is reason `too_large`, and a `507` is reason `quota`. A `422` on commit is a `cache_warning`, with no retry in that run.

#### Scenario: Read-only key
- **WHEN** reserve returns `403` for the first cache saved in a run
- **THEN** that run prints one `cache_warning` about read-only access and attempts no further remote saves

#### Scenario: Quota full
- **WHEN** reserve returns `507`
- **THEN** the local entry is kept and `cache_save_skipped` has reason `quota`

### Requirement: Server unavailable
A `503` from any cache route, a connection failure, or a timeout SHALL turn the remote store off for the rest of the run, with one `cache_remote_unavailable` event. Jobs SHALL then use only the local store.

#### Scenario: No bucket configured
- **WHEN** the server answers the first lookup with `503`
- **THEN** goodflows prints one `cache_remote_unavailable` event and makes no more cache requests in that run

### Requirement: Per-run cache keys on Sprites
When a remote Sprite run has the remote store enabled, the controller SHALL mint a `ck_` key with its own `sk_` key before the remote run starts. The key SHALL have `read_write` access bound to the run's scope, which the controller resolves from its own environment, or `read` when writes are disabled or there is no scope. Its TTL SHALL cover the run, up to 24 hours. The controller SHALL revoke it once the run is collected, cancelled or reconciled.

#### Scenario: Feature-branch Sprite run
- **WHEN** a Sprite run on branch `feat-x` saves a cache
- **THEN** the entry is written to scope `feat-x` with a minted key bound to `feat-x`, and the key is revoked after collection

### Requirement: Minted keys stay private
The controller SHALL forward the minted key only in the private input frame used for forwarded values. It MUST NOT appear in argv, files GoodFlows writes, events, logs, result bundles or the run receipt, and hooks and jobs MUST NOT see it in their environment. The reporting key MUST still never reach the Sprite.

#### Scenario: Key never reaches the job
- **WHEN** a job on the Sprite prints its whole environment and searches the run directory for the minted key
- **THEN** it finds neither the minted key nor the reporting key

### Requirement: Minting failures
When minting fails because there is no `sk_` key, the server errors, or it answers `403`, the Sprite run SHALL continue with only its local store and one `cache_remote_unavailable` event.

#### Scenario: Minting refused
- **WHEN** the controller's key is not `sk_` and minting returns `403`
- **THEN** the run starts, uses the Sprite's local store, and prints one `cache_remote_unavailable` event

### Requirement: Read-only keys for long-lived environments
A run whose cache key is a read-only `ck_` key, such as a long-lived key held by a development environment, SHALL restore from the remote store normally. It treats remote saves as read-only after the first `403`, as above. It still saves to the local store unless `local = false`.

#### Scenario: Long-lived environment warms a cache
- **WHEN** a setup script runs a workflow in a fresh environment whose `GOODFLOWS_CACHE_KEY` is a long-lived read-only `ck_` key bound to the default branch, and CI on the default branch already saved the PLT entry
- **THEN** the PLT is restored from the remote store with `result` `exact`, `mix dialyzer --plt` finds it up to date, and no remote save is attempted
