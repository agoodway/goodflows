---
format: qa-scenarios/2
source: intent
change: "add-stored-changes"
specs: uncommitted
review: null
scenarios:
  - id: 1
    name: "Push refuses a stray file or an invalid change and sends nothing"
    priority: must
    covers:
      - capability: "stored-changes"
        requirement: "change push reads one change directory"
        implied: true
  - id: 2
    name: "Push finds an archived change"
    priority: must
    covers:
      - capability: "stored-changes"
        requirement: "change push reads one change directory"
        implied: true
  - id: 3
    name: "Push sends every file and its provenance in one request"
    priority: must
    covers:
      - capability: "stored-changes"
        requirement: "change push sends the files and where they came from in one request"
        implied: true
      - capability: "change-requests"
        requirement: "Commands use the reporting project, URL and key"
        implied: true
  - id: 4
    name: "Identical push is unchanged, a later push names the version it replaced"
    priority: must
    covers:
      - capability: "stored-changes"
        requirement: "change push reports the version it created or kept"
        implied: true
  - id: 5
    name: "List follows every page and filters by status"
    priority: must
    covers:
      - capability: "stored-changes"
        requirement: "change list shows every stored change"
        implied: true
  - id: 6
    name: "Show prints metadata, an older version, or one file"
    priority: must
    covers:
      - capability: "stored-changes"
        requirement: "change show prints one version or one of its files"
        implied: true
  - id: 7
    name: "Versions lists the history newest first"
    priority: must
    covers:
      - capability: "stored-changes"
        requirement: "change versions lists a change's history"
        implied: true
  - id: 8
    name: "Pull writes a version and refuses over differing local files"
    priority: must
    covers:
      - capability: "stored-changes"
        requirement: "change pull writes a version into the change directory"
        implied: true
  - id: 9
    name: "Pull refuses an archived version"
    priority: must
    covers:
      - capability: "stored-changes"
        requirement: "change pull writes a version into the change directory"
        implied: true
  - id: 10
    name: "Pull refuses a path outside the change files"
    priority: must
    covers:
      - capability: "stored-changes"
        requirement: "change pull writes only change files"
        implied: true
  - id: 11
    name: "A pull that fails partway leaves the directory as it was"
    priority: must
    covers:
      - capability: "stored-changes"
        requirement: "change pull writes all or nothing"
        implied: true
  - id: 12
    name: "Delete needs --yes and names the linking change requests"
    priority: must
    covers:
      - capability: "stored-changes"
        requirement: "change delete needs --yes and names the change requests that block it"
        implied: true
  - id: 13
    name: "Older server says it does not store changes yet"
    priority: must
    covers:
      - capability: "stored-changes"
        requirement: "push and list name a server without stored changes"
        implied: true
  - id: 14
    name: "JSON events, exit codes and offline change commands"
    priority: must
    covers:
      - capability: "stored-changes"
        requirement: "Stored change commands follow the change request conventions"
        implied: true
  - id: 15
    name: "A pk_ key gets the ordinary forbidden error on file contents"
    priority: should
    covers:
      - capability: "stored-changes"
        requirement: "Stored change commands follow the change request conventions"
        implied: true
---

# QA: add-stored-changes

Run these against a GoodFlows Web server with the stored changes routes, a project in `[reporting]`, and an `sk_` key, in a scratch git checkout with a valid change `add-login` in `goodflows/changes/add-login/`. Scenario 13 needs a server without those routes.

## Scenarios

### 1. Push refuses a stray file or an invalid change and sends nothing (Must)

1. Add `notes.txt` to `goodflows/changes/add-login/` and run `goodflows change push add-login`.
   - Expect: exit code 1, the error names `notes.txt`, and `goodflows change list` shows no `add-login`.
2. Remove `notes.txt`, break the change so `goodflows change validate add-login` reports an error (for example, remove a requirement's `Refs:` line), and push again.
   - Expect: exit code 1, and the server still stores no `add-login`.
3. Restore the change, add something that gives only a validate warning (an uncited `by: agent` decision), and push.
   - Expect: exit code 0 and version 1 is created.
4. Copy the change to `goodflows/changes/archive/2026-11-20-add-login/` while keeping `goodflows/changes/add-login/`, and push.
   - Expect: exit code 1, the error names the archived directory, and no new version is created.
5. In a fresh checkout without the archive, with the stored change's latest version archived, write and push a new `goodflows/changes/add-login/`.
   - Expect: exit code 1 saying stored change `add-login` is archived, and no new version is created.

### 2. Push finds an archived change (Must)

1. Push `add-login`, then move it to `goodflows/changes/archive/2026-11-20-add-login/` with its `status` set to `archived`, and run `goodflows change push add-login`.
   - Expect: exit code 0, a new version is created, and `goodflows change show add-login` shows status `archived`.

### 3. Push sends every file and its provenance in one request (Must)

1. Commit `add-login` on branch `plan`, run `goodflows --json change push add-login`, then `goodflows --json change show add-login`.
   - Expect: the stored files are exactly the directory's files, `qa-scenarios.md` included, with identical text; commit is HEAD, branch is `plan`, dirty is false.
2. Edit `change.md` without committing, detach HEAD, and push again.
   - Expect: the new version's branch is null and dirty is true.
3. Run the push with `GOODFLOWS_API_KEY` unset.
   - Expect: exit code 1 naming `GOODFLOWS_URL and GOODFLOWS_API_KEY`, and nothing is sent.

### 4. Identical push is unchanged, a later push names the version it replaced (Must)

1. Run `goodflows --json change push add-login` twice without editing anything.
   - Expect: both exit 0; the second event has `status` `unchanged`, the same `version` as the first, `replaced_version` equal to that version, and `change.status` is the change's `proposed` status.
2. From a second checkout, pull, edit and push (creating version N+1); then edit and push from the first checkout.
   - Expect: the first checkout's push has `status` `created`, `version` N+2 and `replaced_version` N+1, and text output names version N+1.

### 5. List follows every page and filters by status (Must)

1. Push more stored changes than fit on one server page, some `proposed` and one `approved`, and run `goodflows --json change list`.
   - Expect: every pushed change appears once, each with id, status, latest version, pushed time and linked change request numbers.
2. Run `goodflows change list --status approved`.
   - Expect: only the approved change appears.
3. Run `goodflows change list --status done`.
   - Expect: exit code 2 and no request is sent.

### 6. Show prints metadata, an older version, or one file (Must)

1. With `add-login` at version 2, run `goodflows change show add-login --version 1`.
   - Expect: version 1's number, status, provenance and file paths are shown, not version 2's.
2. Run `goodflows change show add-login --file qa-scenarios.md`.
   - Expect: the output is the latest version's `qa-scenarios.md` text.
3. Run `goodflows change show add-login --file nope.md`, then `goodflows change show add-login --version 0`, then `goodflows change show no-such-change`.
   - Expect: exit codes 1 (naming the version's files), 2, and 1.
4. Open a change request with `goodflows cr open --change add-login --title Login` and show the change again.
   - Expect: that change request's number is listed as linking the change.

### 7. Versions lists the history newest first (Must)

1. With `add-login` at three versions, run `goodflows --json change versions add-login`.
   - Expect: three versions in the order 3, 2, 1, each with pushed time, commit, branch and dirty, and version 3 marked latest.

### 8. Pull writes a version and refuses over differing local files (Must)

1. In a fresh checkout without the change, run `goodflows change pull add-login`.
   - Expect: exit code 0, the files match `show` byte for byte, and `git status` shows them untracked, not staged.
2. Add a local `specs/auth/spec.md` and edit `change.md`, then run `goodflows change pull add-login`.
   - Expect: exit code 1, both files are named, and nothing on disk changed.
3. Run `goodflows change pull add-login --force`.
   - Expect: exit code 0, `change.md` matches the stored version, and `specs/auth/spec.md` is gone.
4. Run `goodflows change pull add-login --version 1 --force`.
   - Expect: the directory holds exactly version 1's files.

### 9. Pull refuses an archived version (Must)

1. With the latest stored version of `add-login` archived (scenario 2), run `goodflows change pull add-login --force` in a fresh checkout.
   - Expect: exit code 1 and `goodflows/changes/add-login/` is not created.

### 10. Pull refuses a path outside the change files (Must)

1. Against a stub server whose version holds `change.md` and `specs/../../../.git/config`, run `goodflows change pull add-login --force`; repeat with an absolute path and with `notes.txt`.
   - Expect: each exits 1, and no file is created or changed anywhere, including `goodflows/changes/add-login/change.md` and `.git/config`.

### 11. A pull that fails partway leaves the directory as it was (Must)

1. Make the pull fail after it has written at least one file of the new version (for example by injecting a write failure), then run `goodflows change pull add-login --force` for a version that differs in several files.
   - Expect: exit code 1, and every local file has the same content as before; no file of the new version is present.

### 12. Delete needs --yes and names the linking change requests (Must)

1. Run `goodflows change delete add-login`.
   - Expect: exit code 2 and the change is still listed.
2. With change request #7 linking `add-login`, run `goodflows change delete add-login --yes`.
   - Expect: exit code 1, the message names #7, and the change is still listed.
3. Push a change `drop-me` that no change request links and run `goodflows change delete drop-me --yes`.
   - Expect: exit code 0, and `goodflows change show drop-me` exits 1.

### 13. Older server says it does not store changes yet (Must)

1. Against a server without the stored changes routes, run `goodflows change push add-login` and `goodflows change list`.
   - Expect: both exit 1 saying the server does not store changes yet, not a plain not-found error.
2. Against the same older server, set `[reporting]` to a project that does not exist and run `goodflows change list`.
   - Expect: exit code 1 with the ordinary not-found error.
3. Against a server with the routes, run `goodflows change show no-such-change`.
   - Expect: exit code 1 with the ordinary not-found error, not the "does not store changes" message.

### 14. JSON events, exit codes and offline change commands (Must)

1. Run each of `change push`, `list`, `show`, `pull`, `versions` and `delete --yes` with `--json`.
   - Expect: each prints exactly one line, a JSON object whose `type` is `change_push`, `change_list`, `change_show`, `change_pull`, `change_versions` or `change_delete` and whose `schema_version` is 1.
2. Run `goodflows --json change list` with the server unreachable.
   - Expect: exit code 1 and one `error` event with `code` and `message`.
3. In a checkout with no `goodflows.toml`, run `goodflows change validate add-login`, then `goodflows change list`.
   - Expect: validate exits 0 without a request; list exits 2 for the missing config.

### 15. A pk_ key gets the ordinary forbidden error on file contents (Should)

1. With a `pk_` key, against a server that hides file contents from `pk_` keys, run `goodflows change list`, then `goodflows change show add-login --file change.md` and `goodflows change pull add-login`.
   - Expect: list exits 0; show and pull exit 1 with `forbidden (HTTP 403)`, and pull writes nothing.
