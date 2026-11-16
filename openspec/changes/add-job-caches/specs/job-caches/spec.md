# Spec Delta

## Purpose

Defines job caches: how a job declares them, how keys are built, when goodflows restores and saves them, the local store, and what a run reports. A cache can only make a job faster. It never changes a job's result.

## ADDED Requirements

### Requirement: Jobs declare caches
A command job or step job MAY declare `[[jobs.caches]]` entries, in root `[[jobs]]` and in named workflows. Each cache MUST have a `name` that follows the goodflows name rule and is unique within the job, and a nonempty `paths` list. A workflow-call job MUST NOT declare caches. goodflows SHALL check caches in every workflow when configuration loads, before any job runs, and errors SHALL name the job and the cache.

#### Scenario: Valid cache
- **WHEN** a job declares a cache named `plts` with `paths = ["priv/plts"]` and `key.files = ["mix.lock"]`
- **THEN** `goodflows validate` accepts it and `goodflows list` shows the cache with its paths and key inputs

#### Scenario: Invalid declarations
- **WHEN** a job declares two caches with the same name, a cache with empty `paths`, or a cache on a workflow-call job, in a selected or unselected workflow
- **THEN** validation fails with an error naming the job and the cache, and no job runs

### Requirement: Workdir cache paths
A cache path that does not start with `~/` SHALL be a glob relative to the job's workdir. It MUST NOT be absolute or climb out of the workdir. `key.files` SHALL always be relative to the workdir. goodflows SHALL reject a violation when configuration loads.

#### Scenario: Path outside the workdir
- **WHEN** a cache path is `/tmp/plts` or `../plts`
- **THEN** validation fails before any job runs

### Requirement: Home directory cache paths
A cache path that starts with `~/` SHALL name a directory below the home directory of the user running the job. It MUST NOT be `~/` itself, climb out of the home directory, or name or contain `.ssh`, `.aws` or `.gnupg`. goodflows SHALL reject a violation when configuration loads.

#### Scenario: Package manager cache
- **WHEN** a cache declares `paths = ["~/.cargo/registry", "~/.cargo/git", "target"]`
- **THEN** validation accepts it, and a restore into a fresh machine fills both home directories and `target`

#### Scenario: Sensitive home path
- **WHEN** a cache path is `~/`, `~/../x` or `~/.ssh`
- **THEN** validation fails before any job runs

### Requirement: Cache options
`fallback` SHALL be a boolean that defaults to true. `save` SHALL be `"passed"` (the default) or `"always"`. `timeout_secs` SHALL be a positive integer that defaults to 300. Any other value MUST be rejected when configuration loads.

#### Scenario: Unknown save mode
- **WHEN** a cache sets `save = "never"`
- **THEN** validation fails with an error naming the job and the cache

### Requirement: Key values are expressions
`key.values` and `key.unique` SHALL be lists of expressions that MAY read `env.*` and `needs.JOB.outputs.KEY` for a job in `depends_on`, under the same rules as job conditions. They MUST NOT read `steps.*` or `caches.*`.

#### Scenario: Key value reads a step or an undeclared dependency
- **WHEN** a `key.values` expression reads `steps.build.outputs.x`, or reads `needs.other.outputs.x` where `other` is not in `depends_on`
- **THEN** validation fails before any job runs

### Requirement: Cache keys
After the job's condition passes, goodflows SHALL compute a partial digest over the cache format version, the cache name, the operating system, the CPU architecture and the evaluated `key.values` in order. It SHALL compute the key over the partial digest, a digest of the sorted relative paths and contents of the files matching `key.files`, and the evaluated `key.unique`. No matching file is not an error.

#### Scenario: Lockfile change
- **WHEN** a file that matches `key.files` changes between two runs
- **THEN** the key changes and the partial digest does not

#### Scenario: Different platform
- **WHEN** the same config and files are used on Linux x86_64 and on macOS arm64
- **THEN** the two runs compute different partial digests and never share entries

#### Scenario: Two jobs share a cache
- **WHEN** two jobs declare caches with the same name, the same `key.values` results and the same matching files
- **THEN** they compute the same key and restore the same entry

### Requirement: Unique key parts
`key.unique` values SHALL change the key and MUST NOT change the partial digest, so a run whose key is new restores the newest earlier entry as a fallback and saves a new entry.

#### Scenario: Rolling compiler cache
- **WHEN** a cache declares `paths = ["~/.cache/ccache"]` and `key.unique = ["env.GITHUB_SHA"]`, and CI runs on a new commit
- **THEN** the run restores the previous commit's entry as `fallback`, and after the job passes it saves an entry for the new commit

### Requirement: Bad key values miss
Each `key.values` and `key.unique` result MUST be a scalar. A value that fails to evaluate or is not a scalar SHALL make that cache miss for the run, with a `cache_warning` event, and the job SHALL still run.

#### Scenario: Value fails to evaluate
- **WHEN** a key value is `env.OTP_VERSION < 26` and `OTP_VERSION` is a string, so the comparison fails
- **THEN** goodflows reports a `cache_warning`, skips restore and save for that cache, and runs the job

### Requirement: Restore order
After the job's condition passes and before its command or first step starts, when a declared path is missing, goodflows SHALL look up the exact key in the local store, then in the remote store. When `fallback` is true, it SHALL then look up the newest entry with the same partial digest, locally and then remotely. An entry found remotely SHALL also be stored locally when the local store is enabled.

#### Scenario: Exact hit
- **WHEN** the exact key exists in the local store and `priv/plts` is missing
- **THEN** the entry is extracted before the command starts, and the run reports `exact` from `local`

#### Scenario: Skipped job
- **WHEN** a job's condition is false or a dependency did not pass
- **THEN** goodflows neither restores nor saves its caches

### Requirement: Restore fills only missing paths
goodflows SHALL restore an entry only when at least one declared path is missing, and SHALL then replace every declared path with the entry's contents. When every declared path exists, goodflows SHALL keep the tree and SHALL NOT look up, download or write any entry.

#### Scenario: Rerun in the same checkout
- **WHEN** a job runs again in a checkout where `priv/plts` exists and the exact entry is in the local store
- **THEN** nothing is looked up or extracted, and `priv/plts` is unchanged

#### Scenario: Existing tree with only a fallback stored
- **WHEN** the exact key is not stored, a fallback entry exists, and `priv/plts` already exists
- **THEN** goodflows leaves `priv/plts` unchanged and reports `present`

#### Scenario: Fallback into a fresh tree
- **WHEN** the exact key misses, a fallback entry exists, and `priv/plts` does not exist
- **THEN** goodflows restores the fallback entry and reports `fallback`

### Requirement: Key stamps
goodflows SHALL record, in the `.goodflows/` state directory, the key each cache's tree was built for, per workdir and cache name. It SHALL set the stamp to the entry's key after a restore, and to the computed key when the job ends with a status that `save` accepts, whether or not an entry is written.

#### Scenario: Stamp after a passing job
- **WHEN** a job passes after a miss with `write = false`
- **THEN** nothing is saved and the stamp holds the computed key

### Requirement: Present trees report from their stamp
When every declared path exists, goodflows SHALL report the cache as `exact` with tier `workdir` if its stamp matches the computed key, and as `present` otherwise.

#### Scenario: Lockfile unchanged
- **WHEN** `priv/plts` exists and its stamp matches the key
- **THEN** the cache reports `exact` from `workdir` and `caches.plts.hit` is true

#### Scenario: Lockfile changed
- **WHEN** `priv/plts` exists and `mix.lock` changed since the stamp was written
- **THEN** the cache reports `present`, `caches.plts.hit` is false, and a passing job saves under the new key and updates the stamp

### Requirement: Cache time limits
Restoring and saving a cache SHALL NOT count against the job's `timeout_secs`. Each SHALL have its own bound, the cache's `timeout_secs` (300 seconds by default), applied separately to the restore and to the save. A restore that reaches its bound SHALL count as a miss, and a save that reaches it SHALL count as skipped.

#### Scenario: Slow download
- **WHEN** a remote restore takes longer than the cache's `timeout_secs`
- **THEN** goodflows abandons it, leaves no partial path behind, reports a `cache_warning`, and runs the job as a miss

### Requirement: Steps can read whether a cache hit
Expressions in a job's step conditions and `outputs` SHALL be able to read `caches.NAME.hit`, which is true only when cache NAME of that job is an exact hit: restored from its exact key, or present with a matching stamp. The job's own `condition` MUST NOT read `caches.*`, and an expression MUST NOT name a cache the job does not declare.

#### Scenario: Skip dependency fetch on an exact hit
- **WHEN** a step has `condition = { expression = "!caches.deps.hit" }` and the `deps` cache was restored from its exact key
- **THEN** that step is skipped and the next step runs

#### Scenario: Job condition reads a cache
- **WHEN** a job's `condition` expression reads `caches.deps.hit`
- **THEN** validation fails, because the condition runs before the restore

### Requirement: When a cache is saved
goodflows SHALL save a cache only when it was not an exact hit, its exact key is not already stored, writes are enabled, and the job's status matches `save`. With `save = "passed"` that status is `passed`. With `save = "always"` it is `passed` or `failed`. A job that timed out, was cancelled or was skipped MUST NOT save.

#### Scenario: Passing job after a miss
- **WHEN** the exact key missed and the job passed
- **THEN** the entry is saved and a `cache_saved` event lists the stores it was written to

#### Scenario: Failed job
- **WHEN** the job failed and the cache has `save = "passed"`
- **THEN** nothing is saved and `cache_save_skipped` has reason `not_passed`, while with `save = "always"` the entry is saved

#### Scenario: Exact hit
- **WHEN** the exact key was restored
- **THEN** nothing is saved and `cache_save_skipped` has reason `hit`

#### Scenario: Key already stored
- **WHEN** a present tree's stamp is stale and the exact key is already in the local store
- **THEN** nothing is archived and `cache_save_skipped` has reason `exists`

#### Scenario: Writes disabled
- **WHEN** the selected settings or profile set `[cache] write = false`
- **THEN** caches restore normally, nothing is saved, and `cache_save_skipped` has reason `read_only`

### Requirement: How a cache is saved
A save SHALL archive the declared paths and write the local entry by atomic rename, so a reader never sees a partial entry. When several jobs or partition instances save the same key, the first complete entry SHALL win and the others SHALL count as saved. The save SHALL run in the job's slot after its status is final, and the run SHALL NOT end until every save finishes or reaches its bound.

#### Scenario: Partitions race
- **WHEN** two partition instances save the same key at the same time
- **THEN** the store holds one complete entry and both report `cache_saved`

### Requirement: Caches never change a job's result
A failure to look up, download, verify, restore, archive or save a cache MUST NOT change the job's status, the run's exit code, or when dependents start. goodflows SHALL report it as a `cache_warning`. A failed restore SHALL count as a miss and leave no partially extracted path.

#### Scenario: Corrupt local entry
- **WHEN** a local entry's archive does not match its recorded sha256
- **THEN** goodflows reports a `cache_warning`, does not extract it, treats the cache as a miss, and the job runs

#### Scenario: Save fails
- **WHEN** archiving fails because a path holds a special file
- **THEN** the job keeps its `passed` status and the run exits 0

### Requirement: Saved archives are safe
A cache archive SHALL be a gzip tar holding only the declared paths. Saving MUST refuse special files, and symlinks that point outside the workdir for a workdir path or outside the declared directory for a `~/` path. An archive larger than 5,000,000,000 bytes compressed SHALL NOT be saved, and SHALL be reported as `cache_save_skipped` with reason `too_large`.

#### Scenario: Oversized cache
- **WHEN** the archived paths exceed 5,000,000,000 bytes compressed
- **THEN** nothing is saved and `cache_save_skipped` has reason `too_large`

### Requirement: Restored archives are verified
Restoring MUST verify the archive's sha256 before extracting. It MUST refuse absolute paths, `..` components, entries outside the declared paths, and symlinks that point outside the root they were saved under (the workdir, or the declared `~/` directory), before it writes anything.

#### Scenario: Archive with a path outside the cache
- **WHEN** a downloaded archive holds an entry `../.bashrc`
- **THEN** goodflows writes nothing, reports a `cache_warning`, and treats the cache as a miss

### Requirement: Local store location
Unless `[cache] local = false`, goodflows SHALL keep a private (0700) local store under the user's cache directory (`$XDG_CACHE_HOME/goodflows/` or `~/.cache/goodflows/`, and `~/Library/Caches/goodflows/` on macOS). Checkouts SHALL share one store when they have the same normalized Git remote URL, or the same canonical repository path when there is no remote, or the same config file directory outside Git. On a Sprite, the store SHALL be `$GOODFLOWS_SPRITE_STATE_DIR/caches/`.

#### Scenario: Second worktree
- **WHEN** a job saves an entry in one worktree of a repository and the same job runs in a fresh second worktree
- **THEN** the second run restores the entry from the local store

#### Scenario: Local store disabled
- **WHEN** `[cache] local = false` and the remote store is configured
- **THEN** goodflows restores from and saves to the remote store only, and writes nothing under the user's cache directory

### Requirement: Local store size
goodflows SHALL keep the local store under `[cache] local_bytes` (default 5 GiB) by deleting the least recently used entries after saves. It MUST NOT delete an entry that a running job is reading.

#### Scenario: Store over its cap
- **WHEN** a save brings the store over `local_bytes`
- **THEN** goodflows deletes the least recently used entries until the store fits

### Requirement: Cache settings
A top-level `[cache]` block MAY set `remote`, `token_env`, `write`, `local` and `local_bytes`. Named workflows and environment profiles MAY override `write`, `local` and `local_bytes`, and goodflows SHALL apply them with the same precedence as other settings. Without a `[cache]` block, caches SHALL use only the local store with writes enabled.

#### Scenario: Read-only profile
- **WHEN** `[environments.pr.cache] write = false` and the run uses `--env pr`
- **THEN** caches restore and nothing is saved

### Requirement: Restore events
Each cache of a job that runs SHALL produce one `cache_restored` event with the job, the partition, the cache name, `result` (`exact`, `fallback`, `present` or `miss`), `tier` (`local`, `remote`, `workdir` or null), the size in bytes, and the duration. Text mode SHALL print one line per restore.

#### Scenario: JSON output
- **WHEN** a job with one cache misses, with `--json=summary`
- **THEN** the output has one `cache_restored` event with `result` `miss` and `tier` null for that job and cache

### Requirement: Save events
Each saved cache SHALL produce one `cache_saved` event, and each cache not saved one `cache_save_skipped` event with a reason (`hit`, `exists`, `not_passed`, `read_only`, `no_scope`, `too_large`, `quota` or `error`). Text mode SHALL print one line per save. Run-history records SHALL include each job's cache results.

#### Scenario: Save after a miss
- **WHEN** a job with one cache passes after a miss, with `--json=summary`
- **THEN** the output has one `cache_saved` event for that job and cache, and the history record lists the cache result
