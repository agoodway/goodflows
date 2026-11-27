---
format: goodflows-change/1
id: add-stored-changes
status: approved
size: project
door: one_way
capabilities: [stored-changes, change-requests]
depends_on: []
goals:
  - {id: G1, text: "Agents and teammates see a project's planned changes on the GoodFlows server before anyone opens a branch for them."}
  - {id: G2, text: "Anyone can pick up a stored change into their own checkout and build it from there."}
  - {id: G3, text: "The server copy of a change always mirrors a change directory that passed goodflows' own checks."}
non_goals:
  - {id: N1, text: "Claiming or assigning a stored change to a person or agent."}
  - {id: N2, text: "A guard that refuses a push made from an older version (expected_version)."}
  - {id: N3, text: "cr open pushing the change, or any other change to cr open."}
  - {id: N4, text: "Opening a change request from a stored change, such as seeding its intent set from the stored qa-scenarios.md."}
anti_goals:
  - {id: A1, text: "A pull that overwrites local edits or writes outside the change directory."}
decisions:
  - id: D1
    question: Who owns the server copy of a change?
    answer: "The repository. The server copy is a mirror of the change directory's files: the CLI pushes to it, and nobody edits it on the server."
    by: user
    why: The change files in the repository stay the one source of truth.
    rejected: ["Editing changes on the server", "Two-way sync"]
  - id: D2
    question: What happens to earlier pushes?
    answer: Every push is kept as a new version, so the server holds the history.
    by: user
    why: A reader can see how a planned change evolved, and a push that replaced someone's edits loses nothing.
    rejected: ["Keep only the latest copy"]
  - id: D3
    question: Is qa-scenarios.md stored with the change?
    answer: Yes, it is stored on the server as part of the change.
    by: user
    why: The acceptance scenarios are part of what someone picking up the change builds against.
    rejected: ["Store only change.md and the spec deltas"]
  - id: D4
    question: Can a stored change that a change request links be deleted?
    answer: "No. The server refuses the delete while any change request links it."
    by: user
    why: A change request's link must never point at a change that is gone.
    rejected: ["Delete and leave the link dangling"]
  - id: D5
    question: How does a change request link a stored change, and does cr open change?
    answer: "Through the existing (project, change_id) pair that `goodflows cr open --change <id>` already sends."
    by: user
    why: "The link already exists on the CLI side. Accepted recommendation (Q10): see Approach B; pushing from cr open would also make this change depend on add-change-request-head-attribution."
    rejected: []
  - id: D6
    question: What are the commands?
    answer: "`goodflows change push <id>`, `goodflows change list`, `goodflows change show <id>`, `goodflows change pull <id>`, `goodflows change versions <id>` and `goodflows change delete <id>`, beside the offline validate, verified and archive."
    by: user
    why: "Accepted recommendation (Q1): the commands of Approach A."
    rejected: []
  - id: D7
    question: Which files does a push send?
    answer: "Every file in the change directory, which must be only change.md, specs/<capability>/spec.md, qa-scenarios.md and verification.md. Any other file refuses the push, listing those files, and nothing is sent."
    by: user
    why: "Accepted recommendation (Q2): the server copy is a mirror, so quietly leaving a file out would make it differ from the repository."
    rejected: ["Skip other files with a warning"]
  - id: D8
    question: Does push check the change first?
    answer: "Yes. An active change is pushed only when `goodflows change validate <id>` would report no errors; warnings do not stop it."
    by: user
    why: "Accepted recommendation (Q3): a teammate who picks up a change never gets one that fails validation."
    rejected: ["Push whatever is on disk and let the server check"]
  - id: D9
    question: Does push read a commit or the working tree?
    answer: "The working tree: files as they are on disk, committed or not, on a branch or with HEAD detached."
    by: user
    why: "Accepted recommendation (Q4): the point is sharing before any branch exists, so requiring a commit gets in the way."
    rejected: ["Push the change as committed at HEAD"]
  - id: D10
    question: Does a push say where it came from?
    answer: "Yes. Each push sends HEAD's commit, the current branch (null when detached), and dirty: whether any of the change's files differ from HEAD or are untracked."
    by: user
    why: "Accepted recommendation (Q5): a reader can tell a push from main at a commit from someone's uncommitted edits."
    rejected: ["Send nothing beyond the files"]
  - id: D11
    question: What does an identical push do?
    answer: "When the files match the latest stored version byte for byte, the server keeps that version; push reports status unchanged with that version number and exits 0."
    by: user
    why: "Accepted recommendation (Q6): the same as Git facts reports and claims uploads, which report unchanged, and scenario set uploads, which report a duplicate; it makes re-running a push safe."
    rejected: ["Create a new version anyway", "Fail on an identical push"]
  - id: D12
    question: What happens when two people push the same change?
    answer: "The last push wins and becomes the next version. The push output names the version it replaced, so the pusher can see that someone else pushed in between."
    by: user
    why: "Accepted recommendation (Q7): every version is kept, so nothing is lost; a guard needs local state outside the change directory and a server check, and can come later."
    rejected: ["Refuse a push made from an older version"]
  - id: D13
    question: What does pull do when local files exist?
    answer: "When goodflows/changes/<id>/ has a file whose content differs from the version, or a file the version does not have, pull refuses, lists those files and writes nothing, unless --force. With --force it writes the version and deletes local files the version does not have."
    by: user
    why: "Accepted recommendation (Q8): pulling over uncommitted edits is the one way this feature can lose work."
    rejected: ["Always overwrite", "Merge"]
  - id: D14
    question: How are archived changes handled?
    answer: "push <id> reads goodflows/changes/<id>/ and, when that does not exist, the archived directory goodflows/changes/archive/<date>-<id>/, so the server copy shows archived. pull refuses a version whose status is archived."
    by: user
    why: "Accepted recommendation (Q9): without the archive lookup a stored change stays approved forever, and picking up a finished change is not the use case."
    rejected: ["Push only active changes", "Pull archived versions into goodflows/changes/<id>/"]
  - id: D15
    question: What does show print?
    answer: "The stored change's metadata (version, status, when and from where it was pushed, linked change requests) and its file list. --version N picks an older version; --file <path> prints that one file's text instead."
    by: user
    why: "Accepted recommendation (Q11): an agent can read a change without pulling it into a checkout."
    rejected: []
  - id: D16
    question: What does list print?
    answer: "Every stored change in the project, following every page, one line each with id, status, latest version, when it was pushed and the linked change request numbers. --status proposed|approved|archived filters; the default shows all."
    by: user
    why: "Accepted recommendation (Q12): seeing the planned changes is the first goal."
    rejected: []
  - id: D17
    question: Where does pull write?
    answer: "`goodflows change pull <id> [--version N]` writes the version (default: the latest) to goodflows/changes/<id>/ under the checkout's top level, and does not stage anything in git."
    by: user
    why: "Accepted recommendation (Q13): that is where every other change command reads a change, and committing stays the person's choice."
    rejected: []
  - id: D18
    question: How is a delete confirmed, and how is a refusal shown?
    answer: "delete needs --yes and exits 2 without it, sending nothing. When the server refuses because change requests link the change, it exits 1 naming those change requests."
    by: user
    why: "Accepted recommendation (Q14): a delete removes every version for good; --yes stops an accidental delete without ever prompting an agent."
    rejected: ["An interactive prompt", "No confirmation"]
  - id: D19
    question: What are the JSON events?
    answer: "One event per command, with types change_push, change_list, change_show, change_pull, change_versions and change_delete and schema_version 1. change_push carries id, version, status (created or unchanged) and replaced_version."
    by: user
    why: "Accepted recommendation (Q15): the same one-event convention as the cr commands; other fields keep the server's names."
    rejected: []
  - id: D20
    question: Which exit codes do the commands use?
    answer: "No new ones: 0 on success, 2 for usage or config errors (including a missing --yes), 1 for every other failure, including an unknown change id."
    by: user
    why: "Accepted recommendation (Q16): the same codes as every cr command."
    rejected: []
  - id: D21
    question: What happens against a server without stored changes?
    answer: "push and list say that the GoodFlows server does not store changes yet, and exit 1."
    by: user
    why: "Accepted recommendation (Q17): the CLI can ship before the server, and a bare 404 would read as a missing project."
    rejected: ["Show the plain 404"]
  - id: D22
    question: Which paths may pull write?
    answer: "Only change.md, specs/<capability>/spec.md, qa-scenarios.md and verification.md inside the change directory. A version with any other path, including one with .. or an absolute path, is refused whole and nothing is written."
    by: user
    why: "Accepted recommendation (Q18): paths come from the server, and a bad answer must never write outside the change directory."
    rejected: ["Skip the bad paths and write the rest"]
  - id: D23
    question: Can a failed pull leave a half-written directory?
    answer: "No. A pull writes the whole version or leaves the local directory exactly as it was, with or without --force."
    by: user
    why: "Accepted recommendation (Q19): a half-written change mixes two versions and loses the local copy."
    rejected: []
  - id: D24
    question: What if a push's response is lost?
    answer: "No request is retried and no idempotency key is sent."
    by: user
    why: "Accepted recommendation (Q20): the identical-push rule already makes a re-run safe, and every cr command already never retries."
    rejected: ["Retry with an idempotency key"]
  - id: D25
    question: Does the CLI enforce the server's size limits?
    answer: "No. The server's limits (proposed: 100 files and 2 MB per push) are the server's; its refusal comes through as an ordinary server error."
    by: user
    why: "Accepted recommendation (Q21): repeating the limits makes the CLI drift when the server changes them."
    rejected: ["Check the limits locally before sending"]
  - id: D26
    question: What does a key that cannot read file contents see?
    answer: "The server's 403 as the ordinary `<command> failed: forbidden (HTTP 403)` error, with no special message."
    by: user
    why: "Accepted recommendation (Q22): the CLI leaves key checks to the server, as the change-requests spec already says."
    rejected: []
  - id: D27
    question: Which capabilities hold the requirements?
    answer: "A new stored-changes capability for the six commands, and a MODIFIED change-requests requirement adding them to the commands that use the [reporting] project, URL and key."
    by: agent
    why: "The [reporting] rules live in change-requests and should name every command that follows them; the six commands are a capability of their own. goodflows change validate, verified and archive have no capability spec and stay out of this change."
    rejected: ["Put everything in change-requests"]
  - id: D28
    question: What does versions print?
    answer: "Newest first, with the version number, when it was pushed, its commit, branch and dirty flag, and which version is the latest."
    by: agent
    why: The user only asked for a way to list versions; this is the smallest listing that answers which push came from where.
    rejected: []
  - id: D29
    question: Do archived directories go through change validate before a push?
    answer: "No. An archived directory is checked only for the allowed file kinds; change validate is skipped for it."
    by: agent
    why: "change validate is written for active changes: an archived change's ADDED requirements are already in goodflows/specs/, so it would report errors for every archived change and D14 could never work."
    rejected: ["Validate archived changes too, which refuses them all"]
  - id: D30
    question: Which server contract does this change assume?
    answer: "The goodflows_web change add-stored-changes (status approved, 2026-11-27). POST /api/v1/projects/:project/changes with exactly {\"files\": {\"<path>\": \"<text>\"}, \"source\": {\"commit\", \"branch\", \"dirty\"}}; commit is 7-64 lowercase hex or null, branch 1-256 bytes without control characters or null; the id comes only from change.md. Paths are change.md (required), qa-scenarios.md, verification.md and specs/<cap>/spec.md with segments matching [a-z0-9][a-z0-9-]*, each path at most 512 bytes. A new version answers 201; an identical push answers 200 with duplicate true; replaced_version is the version that was latest before the push, the latest version itself for a duplicate and null for a new change. Refusals: 413 over 16 MiB; 422 for the body, paths, limits (100 files, 2,097,152 bytes), change.md and qa-scenarios.md; 409 when the latest version is archived. GET /changes and GET /changes/:id/versions page with limit, cursor and next_cursor; /changes takes status; version entries carry number, status, pushed_at, source, user_id and latest; GET /versions/:n adds latest_status. GET /changes/:id returns the latest version with files and linked change requests. A pk_ key gets 403 on the change and single-version reads, also for an unknown id, and 200 on both lists. DELETE answers 204 with no body, 404 for an unknown id, and 409 listing the linking change requests' numbers and states."
    by: agent
    why: The server change owns the HTTP contract and reported it to this thread; it is not yet approved, so the builder follows the contract the server ships and records each difference as a Found while building decision.
    rejected: []
  - id: D31
    question: How does the CLI tell a server without stored changes from a missing project?
    answer: "When change push or change list gets a 404 from the /changes collection, it sends one GET of the project's change requests, a route every released server has. A 2xx means the server does not store changes yet (D21); anything else reports the original 404. This probe is a different request, not a retry of the push."
    by: agent
    why: "Released GoodFlows Web servers answer an unknown route and an unknown project with the same JSON 404 body, so the body cannot tell them apart; the probe works against servers already deployed. A distinguishable error from the new server is asked of the server thread, and does not help older servers."
    rejected: ["Read the 404 body, which released servers make identical", "Show the plain 404"]
  - id: D32
    question: What does push do when several archived directories match the id?
    answer: Exit 1 naming every matching directory and send nothing.
    by: agent
    why: change archive allows archiving the same id on different dates, and guessing the newest would mirror the wrong change.
    rejected: ["Push the newest by date"]
  - id: D33
    question: What does pull do when the checkout already has an archived directory for the id?
    answer: "Exit 1 and write nothing, even with --force, naming the archived directory."
    by: agent
    why: "Pulling a stale approved copy beside its archive would create an active directory that the next push prefers (D14), un-archiving the change on the server."
    rejected: ["Pull anyway"]
  - id: D34
    question: Do the change commands accept --config?
    answer: "push, list, show, pull, versions and delete accept --config as cr does; validate, verified and archive keep refusing it with exit 2."
    by: agent
    why: The server commands load the config for [reporting]; the offline commands read no config, and refusing it there is today's behavior.
    rejected: ["Refuse --config for all change commands"]
  - id: D35
    question: What provenance does a push send from a repository with no commit yet?
    answer: "commit null, the unborn branch's name as branch, and dirty true."
    by: agent
    why: Sharing before any branch exists (G1) includes a fresh repository; failing there would block the use case.
    rejected: ["Refuse to push without a commit"]
  - id: D36
    question: Does an identical push update the stored provenance?
    answer: "No. Only the files decide whether a push is unchanged; the kept version keeps the provenance of the push that created it."
    by: agent
    why: Versions are immutable history (D2); rewriting provenance on an unchanged push would make a version say it came from a push that did not create it.
    rejected: ["Create a new version when only the provenance differs"]
  - id: D37
    question: What does push do with an archived file that is not UTF-8?
    answer: Exit 1 naming the file and send nothing.
    by: agent
    why: "Files travel as JSON text, so such a file cannot be mirrored exactly (D1); active changes already fail validate for it."
    rejected: ["Send it lossily"]
  - id: D38
    question: Can pull --version fetch an older, non-archived version of a change whose latest version is archived?
    answer: "No. pull refuses every version of a stored change whose latest version is archived."
    by: agent
    why: "Found by the builder check: a per-version check lets pull --version recreate an active directory, and the next push un-archives the change, the failure D33 prevents."
    rejected: ["Check only the pulled version's status"]
  - id: D39
    question: Which specs paths do push and pull allow?
    answer: "The same rule for both: specs/<capability>/spec.md where <capability> follows change validate's capability name rule (including area/name), with no empty, . or .. segment."
    by: agent
    why: "If push and pull read the rule differently, a change can be pushed that nobody can pull; one rule also keeps D22's path check tight."
    rejected: ["Only one-segment capability names"]
  - id: D40
    question: When is a push dirty?
    answer: "dirty is false only when the directory's files are exactly the files HEAD tracks under that path, with the same content. A deleted tracked file, an untracked file and a git-ignored file all make it true."
    by: agent
    why: "dirty: false is a promise that the named commit holds these files (D10); any other definition lets a reader trust a false provenance."
    rejected: ["Differs from HEAD or untracked, ignoring deleted and ignored files"]
  - id: D41
    question: Does an interrupted pull count as a failed one?
    answer: "Yes. A pull interrupted by a signal leaves the directory as it was (D23) and exits 128 plus the signal number."
    by: agent
    why: "Ctrl-C during the write is the likeliest way a pull fails partway; today change commands die on a signal without cleanup."
    rejected: []
  - id: D42
    question: How do push and pull treat symlinks?
    answer: "Both refuse, exit 1 and send or write nothing, when the change directory, any path component under the checkout's goodflows/changes, or any file in the change is a symlink."
    by: agent
    why: "Following one lets push send text that is not in the change (D1) or lets pull write or delete outside the change directory (A1), which path checks alone do not catch."
    rejected: ["Follow symlinks as files"]
  - id: D43
    question: What does pull do with a file that comes back without text?
    answer: "Refuse the whole version, exit 1 and write nothing, as for a bad path."
    by: agent
    why: "A server that redacts contents instead of answering 403 would otherwise make pull --force write empty files over real ones."
    rejected: []
  - id: D44
    question: What does push require of an archived directory besides allowed, UTF-8 files?
    answer: "A change.md whose front matter is readable and whose id is the requested id."
    by: agent
    why: "Without it an archived push could mirror a directory with no status, breaking D14's promise that the server shows archived."
    rejected: []
  - id: D45
    question: What does push do when an active directory and an archived directory share the id?
    answer: "Exit 1 and send nothing, naming the archived directory and saying to pick a new id. Each stored id is one change."
    by: user
    why: "Accepted recommendation (Q23): otherwise the new change becomes the next version of the archived change and un-archives it on the server, or, under D33, nobody whose checkout has the archive can pull it."
    rejected: ["Append the new change to the archived change's history"]
  - id: D46
    question: Where does change_push carry the stored change's own status?
    answer: "Under a change key holding the server's stored-change object, so change.status is proposed, approved or archived and the top-level status stays the push result (created or unchanged)."
    by: user
    why: "Accepted recommendation (Q24): the server's change status and the push result would otherwise collide under one name in a schema_version 1 event."
    rejected: ["Rename the change's status to change_status"]
  - id: D47
    question: What does push do on a branch name the server refuses (over 256 bytes or with control characters)?
    answer: "Exit 1 and send nothing, naming the branch limit, as cr open does."
    by: agent
    why: "The server answers 422 for such a branch (D30); sending null instead would misstate where the push came from (D10)."
    rejected: ["Send branch null"]
  - id: D48
    question: How does push report the server's 409 for a change whose latest stored version is archived?
    answer: "Exit 1 saying the stored change <id> is archived and to pick a new id for new work; other 409s keep the generic message."
    by: agent
    why: "A checkout without the archived directory passes D45 locally, and the server's refusal is then the only guard; the known-409 rule asks for a next step."
    rejected: ["Show the generic conflict"]
  - id: D49
    question: What does a successful delete need from the response?
    answer: "Nothing: delete treats the server's 204 with no body as success, unlike other commands, which need data in a 2xx body."
    by: agent
    why: "The server answers DELETE with 204 and no body (D30); the change-requests transport rule would otherwise fail it as having no data."
    rejected: []
  - id: D50
    question: What is replaced_version on an unchanged push?
    answer: "The server's value: the version that was latest before the push, which for an unchanged push is the kept version itself. It is null only for a new change."
    by: user
    why: "Accepted recommendation (Q25): the server chose this for its own replaced_version, and one field name meaning two things across the CLI and the server is a trap for scripts."
    rejected: ["Report null on an unchanged push"]
  - id: D51
    question: What does pull do when the server's version does not say the latest version's status?
    answer: "Exit 1 and write nothing, as for an archived change: the latest version's status is status for the latest version and latest_status for --version N, and a missing value is refused."
    by: agent
    why: "Found while building: without the status pull cannot tell whether the change is archived (D38), and guessing active could un-archive it on the next push."
    rejected: ["Treat a missing status as not archived"]
  - id: D52
    question: Where does show --version N get the linked change requests?
    answer: "From one more read, GET /changes/:id, after GET /changes/:id/versions/:n; both are private-key reads."
    by: agent
    why: "Found while building: the shipped server's version read (StoredChangeJSON.show_version) carries files and latest_status but no change_requests, so D15's linked change requests for an older version need the change read."
    rejected: ["Show no linked change requests for an older version"]
  - id: D53
    question: What does the change_push event's change object hold?
    answer: "The server's push answer (change_id, version, status, size, door, capabilities, depends_on, pushed_at, source, pushed_by) without duplicate and replaced_version, which the event carries as status and replaced_version at the top level."
    by: agent
    why: "Found while building: the shipped server answers a push with one flat version object, not a separate stored-change object, and keeping the push-result fields out of change keeps change.status the change's own status (D46)."
    rejected: ["Copy the whole answer, duplicate and replaced_version included"]
  - id: D54
    question: What does pull do when the version lists the same path twice?
    answer: "Refuse the whole version, exit 1, name the path and write nothing, as for a bad path."
    by: agent
    why: "Found while building: the shipped server sends a version's files as an array of {path, text}, not an object, so a path can repeat, and picking one copy would write text the version may not mean."
    rejected: ["Keep the last copy"]
  - id: D55
    question: Where does change list get each change's linked change requests?
    answer: "From the change_requests ([{number, state}]) of each GET /changes row, which pk_ keys may read too; a row without the field shows no numbers (-), as does a row whose list is empty, and the command still succeeds."
    by: agent
    why: "Found while building: the first shipped list rows carried no linked change requests; the server thread then amended GET /changes to carry change_requests on each row, and servers already deployed still leave the field out, so list must not depend on it or read each change per row (a pk_ key gets 403 there)."
    rejected: ["GET /changes/:id per row", "Fail when a row has no change_requests"]
  - id: D56
    question: Which fields name a version entry's number and pusher?
    answer: "The server's: each entry of GET /changes/:id/versions carries its number as version and its pusher as pushed_by.user_id, not number and user_id as D30 says. versions sorts and prints by version, and the change_versions event passes the entries through with the server's names."
    by: agent
    why: "Found while building: the shipped server (StoredChangeJSON.data/2) renders version entries with version and pushed_by: {user_id}, and D19 keeps the server's field names in the events."
    rejected: ["Rename the fields to number and user_id in the event"]
left_to_builder:
  - "Wording of errors a person reads, with the bar that each names the input or state at fault (the change id, the file, the version) and what to do next."
  - "How each requirement is tested, with the bar that breaking the behavior of any Must scenario fails an automated test."
  - "Wording and layout of text output for every command except list, whose one line per change D16 pins, as long as list, show and versions show the fields their decisions name and push names the replaced version."
  - "Fields of the JSON events beyond those D19 pins, keeping the server's field names."
  - "How pull makes its write all or nothing (D23), with the constraint that no temporary directory or file is ever left where change validate or discovery under goodflows/changes/ would read it as a change."
  - "Whether show --file prints the file's text unescaped in text output; --json carries it escaped like every other value."
  - "What a pull whose version matches the local files exactly prints, as long as it writes nothing and exits 0."
---

## Why

tbrewer wants agents and teammates to see planned changes, and pick them up, before anyone opens a branch. Today a goodflows change lives only in whichever checkout or branch it was written in, so nobody else knows it exists until a change request links it.

## Context

`goodflows change validate`, `change verified` and `change archive` (`src/change_requests/changes.rs`) work on `goodflows/changes/<id>/` without a server, `goodflows.toml` or a key, and `src/main.rs` refuses `--config` for every `change` command. The new commands are the first `change` commands that talk to the server, so they load the config and use `[reporting]` like the `cr`, `policy`, `features` and `events` commands, which share the HTTP client, error messages and JSON event conventions in `src/change_requests/api.rs` and the `change-requests` spec.

`goodflows cr open --change <id>` already sends `change_id` when it creates a change request, and syncs its dependencies from `change.md`.

`goodflows features sync` is the nearest existing pattern: read local files, check them, send one `POST`, and print the new version.

The durable `change-requests` spec still names `--openspec-change` and `openspec_change`, which commit f80364a renamed to `--change` and `change_id` in the code. This change does not touch those requirements. The in-flight `add-change-request-head-attribution` modifies the `cr open` requirement; this change modifies a different `change-requests` requirement, so it does not depend on it.

The server side is a separate goodflows change in agoodway/goodflows_web, which owns the HTTP contract. D30 records the contract this change assumes.

## Approaches

- **A. Explicit `goodflows change` subcommands (picked).** Sharing is one deliberate step, the offline commands stay offline, and pull lets a reader pick a change up without copying files out of JSON. The cost is that someone has to remember to push, so the server copy can go stale.
- **B. Push automatically from `cr open --change`** (and maybe `change archive`). Keeps the copy current without thought, but runs only once a change request exists, which misses the before-a-branch case, and makes `cr open` slower and able to fail in new ways. Considered as an add-on to A and left out.
- **C. Share through git**, with a pushed or planning branch. Needs a branch first, which is what the user wants to avoid, and gives nobody a list of planned changes. Dropped.
