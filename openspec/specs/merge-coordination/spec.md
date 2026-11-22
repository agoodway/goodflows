# merge-coordination Specification

## Purpose

Defines how `goodflows cr lease` acquires, renews and releases the merge slot lease of a target branch, how it picks the base commit, and how it explains a refused lease.

## Requirements

### Requirement: cr lease acquire asks for the merge slot at HEAD
`goodflows cr lease acquire` SHALL pick the change request as the change-requests capability specifies and send `POST change-requests/<N>/merge-lease` with JSON body `{"head": <HEAD>, "base": <base>}`, where `head` is the full SHA of this checkout's `HEAD` commit and `base` is the full SHA of the target branch commit the merge goes onto.

#### Scenario: Acquire for change request 7
- **WHEN** a user runs `goodflows cr lease acquire -n 7 --base main`
- **THEN** goodflows sends `POST /api/v1/projects/<project>/change-requests/7/merge-lease` whose `head` is the output of `git rev-parse HEAD` and whose `base` is the output of `git rev-parse main`

### Requirement: --base names the base commit
With `--base REV`, goodflows SHALL resolve REV to a full commit SHA in this checkout, without reading the change request or fetching. When REV names no commit it SHALL fail with exit 1 and a message starting `<REV> does not name a commit`, and send no lease request. `--base` and `--no-fetch` SHALL NOT be given together; that is a usage error with exit 2.

#### Scenario: A revision expression
- **WHEN** a user runs `goodflows cr lease acquire -n 42 --base 'main~0'` while `origin/main` points at a different commit than `main`
- **THEN** goodflows sends only the lease request, and its `base` is the SHA of local `main`, not of `origin/main`

#### Scenario: Unknown revision
- **WHEN** a user runs `goodflows cr lease acquire -n 42 --base nope`
- **THEN** goodflows prints an error starting `nope does not name a commit`, sends no `merge-lease` request and exits 1

#### Scenario: Both flags
- **WHEN** a user runs `goodflows cr lease acquire --base main --no-fetch`
- **THEN** goodflows reports that `--base <REV>` cannot be used with `--no-fetch` and exits 2

### Requirement: The default base is the target branch, origin first
Without `--base`, goodflows SHALL read `GET change-requests/<N>` for its `target_branch`, then use `refs/remotes/origin/<target>` when this checkout has it, else `refs/heads/<target>`. When it has neither it SHALL fail with exit 1: `the target branch <target> is not in this checkout as origin/<target> or <target>; fetch it first`.

#### Scenario: origin/main ahead of main
- **WHEN** change request 42 targets `main`, `origin/main` and `main` point at different commits, and a user runs `goodflows cr lease acquire -n 42 --no-fetch`
- **THEN** the lease request's `base` is the SHA of `origin/main`

#### Scenario: Only a local branch
- **WHEN** change request 42 targets `main`, the checkout has no `origin/main`, and a user runs `goodflows cr lease acquire -n 42 --no-fetch`
- **THEN** the lease request's `base` is the SHA of local `main`

### Requirement: The default base is fetched first unless --no-fetch
Without `--base` or `--no-fetch`, when the checkout has an `origin` remote, goodflows SHALL run `git fetch --quiet origin +refs/heads/<target>:refs/remotes/origin/<target>` with `GIT_TERMINAL_PROMPT=0` before choosing the base. A failed fetch SHALL fail with exit 1, `git fetch origin <target> failed: <git stderr>; pass --no-fetch to use the refs this checkout has`, and no lease request. Without `origin` it SHALL not fetch.

#### Scenario: Unreachable origin
- **WHEN** change request 42 targets `main`, `origin` points at a path that is not a repository, and a user runs `goodflows cr lease acquire -n 42`
- **THEN** goodflows prints an error starting `git fetch origin main failed:` and ending `; pass --no-fetch to use the refs this checkout has`, sends no `merge-lease` request and exits 1

#### Scenario: Skip the fetch
- **WHEN** the same user runs `goodflows cr lease acquire -n 42 --no-fetch`
- **THEN** goodflows runs no `git fetch` and sends the lease request with the base it finds locally

### Requirement: A taken slot names its holder
When acquire fails with `409` and `errors.error` `slot_taken`, goodflows SHALL fail with exit 1 and print `cr lease acquire failed: another lease holds the merge slot of the target branch: change request #<holder.change_request> holds it until <holder.expires_at>; wait for it to merge or expire, then acquire again`. Without a `holder` object the message SHALL end after `target branch` with `; acquire again`.

#### Scenario: Holder known
- **WHEN** the server responds `409` with `{"errors": {"error": "slot_taken", "holder": {"change_request": 3, "expires_at": "2026-11-21T10:10:00Z"}}}`
- **THEN** goodflows prints a message containing `cr lease acquire failed: another lease holds the merge slot` and `change request #3 holds it until 2026-11-21T10:10:00Z`, and exits 1

#### Scenario: Holder unknown
- **WHEN** the server responds `409` with `{"errors": {"error": "slot_taken", "holder": null}}`
- **THEN** goodflows prints `cr lease acquire failed: another lease holds the merge slot of the target branch; acquire again`

### Requirement: Acquire explains why the change request cannot merge
For a `409` acquire, goodflows SHALL print `cr lease acquire failed: ` and, by `errors.error`: `not_mergeable`, `change request #<N> is not mergeable (verdict <errors.state>); see goodflows cr verdict`; `stale_head`, `HEAD is not the reported head of change request #<N>, which is <errors.head>; run goodflows cr push or goodflows cr claims, or check out that head`; `not_open`, `change request #<N> is not open; it must be ready first (goodflows cr ready)`.

#### Scenario: Not mergeable
- **WHEN** change request 42's verdict is `needs_review` and the server responds `409` with `{"errors": {"error": "not_mergeable", "state": "needs_review"}}`
- **THEN** goodflows prints `cr lease acquire failed: change request #42 is not mergeable (verdict needs_review); see goodflows cr verdict` and exits 1

#### Scenario: Draft change request
- **WHEN** the server responds `409` with `{"errors": {"error": "not_open"}}` for change request 42
- **THEN** goodflows prints `cr lease acquire failed: change request #42 is not open; it must be ready first (goodflows cr ready)`

### Requirement: Other acquire failures print as server errors
A `409` whose `errors.error` is not `slot_taken`, `not_mergeable`, `stale_head` or `not_open`, and any other failed status, SHALL print as the change-requests capability specifies for server errors, naming `cr lease acquire`, and exit 1.

#### Scenario: Unknown conflict code
- **WHEN** acquire responds `409` with `{"errors": {"detail": "x", "error": "something_new"}}`
- **THEN** goodflows prints `cr lease acquire failed: conflict (HTTP 409): ` followed by the response body, and exits 1

### Requirement: cr lease heartbeat extends a lease by id
`goodflows cr lease heartbeat <LEASE_ID>` SHALL send `POST merge-leases/<LEASE_ID>/heartbeat` with no body. It SHALL take no change request number and look up no branch, so it sends exactly that one request.

#### Scenario: Heartbeat
- **WHEN** a user runs `goodflows cr lease heartbeat 7c0e`
- **THEN** goodflows sends exactly one request, `POST /api/v1/projects/<project>/merge-leases/7c0e/heartbeat`

### Requirement: cr lease release frees a lease by id
`goodflows cr lease release <LEASE_ID>` SHALL send `DELETE merge-leases/<LEASE_ID>` and nothing else. On success it SHALL print the lease as `{"id": <LEASE_ID>, "state": "released"}`, built by goodflows whatever the success body is, so a `204` with no body prints the same as any other success.

#### Scenario: Release with 204
- **WHEN** a user runs `goodflows cr lease release 7c0e --json` and the server responds `204` with no body
- **THEN** goodflows sends `DELETE /api/v1/projects/<project>/merge-leases/7c0e`, prints a JSON event with `type` `merge_lease`, `action` `release` and `lease` `{"id": "7c0e", "state": "released"}`, and exits 0

#### Scenario: Release as text
- **WHEN** the same release runs without `--json`
- **THEN** goodflows prints `id: 7c0e` and `state: released`

### Requirement: A lease that is no longer active says so
When heartbeat or release fails with `409` and `errors.error` `lease_not_active`, goodflows SHALL fail with exit 1 and print `cr lease <heartbeat|release> failed: lease <LEASE_ID> is no longer active (unknown, released or expired); acquire a new one with goodflows cr lease acquire`. Other failures SHALL print as the change-requests capability specifies.

#### Scenario: Releasing twice
- **WHEN** lease `7c0e` was already released and a user runs `goodflows cr lease release 7c0e`
- **THEN** goodflows prints `cr lease release failed: lease 7c0e is no longer active (unknown, released or expired); acquire a new one with goodflows cr lease acquire` and exits 1

#### Scenario: Heartbeat after expiry
- **WHEN** lease `7c0e` expired and a user runs `goodflows cr lease heartbeat 7c0e`
- **THEN** the message starts `cr lease heartbeat failed: lease 7c0e is no longer active`

### Requirement: Lease commands print the lease
A successful lease command SHALL print one `merge_lease` event with `action` (`acquire`, `heartbeat` or `release`) and `lease`: the response's `data` for acquire and heartbeat, and exit 0. The text output SHALL print the lease's fields as `key: value` lines in key order, nested values indented.

#### Scenario: Heartbeat as text
- **WHEN** a heartbeat for `7c0e` responds `200` with `{"data": {"id": "7c0e", "state": "active", "expires_at": "2026-11-21T10:20:00Z"}}`
- **THEN** goodflows prints `expires_at: 2026-11-21T10:20:00Z`, `id: 7c0e` and `state: active`, in that order, and exits 0

#### Scenario: Acquire as JSON
- **WHEN** acquire for change request 42 responds with lease `{"id": "7c0e", "state": "active"}` and the user passed `--json`
- **THEN** goodflows prints a JSON event with `type` `merge_lease`, `action` `acquire` and that `lease`
