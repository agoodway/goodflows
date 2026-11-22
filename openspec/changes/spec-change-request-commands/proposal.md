# Proposal

## Why

The change request, review policy, feature index and events commands landed in c2a9767 and later commits without OpenSpec specs. `openspec/specs/` covers only `job-caches`, `remote-cache`, `run-history` and `secrets`, so a change to these commands, such as rendering the calibration report's new defect list (goodflows_web `add-calibration-defect-list`), has no spec to modify and no stated contract to check against.

## What Changes

- No code changes. This change specifies the commands as they behave on `main` at 4ba3522, so archiving it creates their specs.
- Behavior that looks wrong is recorded in design.md under "Found while specifying" and is not specified as intended. Each item gets its own change.

## Capabilities

### New Capabilities

- `change-requests`: the shared rules (reporting config, keys, picking the change request, `--json`, error messages, fetching the target), the claims file and Git facts, and `cr open`, `claims`, `push`, `status`, `path`, `validate`, `verdict`, `wait`, `ready`, `draft`, `close`, `reopen`, `merged`, `retarget` and `parent`.
- `qa-scenario-sets`: `cr scenarios` and the `qa-scenarios/2` set file as the CLI reads it.
- `verify-runs`: `cr runs upload` and the run folder.
- `change-request-review`: `cr review`, `cr answer` and `cr tasks`.
- `merge-coordination`: `cr lease acquire|heartbeat|release`.
- `project-events`: `goodflows events`.
- `review-policy`: `policy get`, `set` and `history`.
- `review-calibration`: `cr defect` and `policy calibration`.
- `feature-index`: `features sync` and `features validate`.

The capability names match goodflows_web's, so a reader can find the server side of each command under the same name.

## Impact

- Docs only: `openspec/changes/spec-change-request-commands/`.
- Later changes to these commands, starting with the calibration defect list, modify these specs.
