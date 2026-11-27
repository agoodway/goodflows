## ADDED Requirements

### Requirement: change push reads one change directory
Refs: D7, D8, D9, D14, D29, D32, D37, D39, D42, D44, D45, G3
`goodflows change push <id>` SHALL read the files on disk in `goodflows/changes/<id>/`, committed or not, or, when that directory does not exist, in the one directory under `goodflows/changes/archive/` whose name is a date followed by `-<id>`. It SHALL exit 1 and send nothing when neither exists, when several archived directories match (naming each), when both `goodflows/changes/<id>/` and an archived directory for the id exist (naming the archived directory), when the directory holds any file other than `change.md`, `specs/<capability>/spec.md`, `qa-scenarios.md` and `verification.md` (naming each such file), when the directory or any path in it is a symlink, or when an active change has errors under the same checks as `goodflows change validate <id>`. Validation warnings SHALL NOT stop a push, and an archived directory SHALL be checked only for the allowed files, that each is UTF-8 text, and that its `change.md` has readable front matter whose `id` is `<id>`.

### Requirement: change push sends the files and where they came from in one request
Refs: D1, D2, D3, D10, D24, D25, D30, D35, D40, D47, G1
`change push` SHALL send one `POST .../changes` carrying every file of the directory, each by its path relative to the directory and its exact text, including `qa-scenarios.md`, together with HEAD's commit (null when the branch has no commit yet), the current branch (null when HEAD is detached) and `dirty`, which is false only when the directory's files are exactly the files HEAD tracks under that path, with the same content. It SHALL exit 1 and send nothing when the branch name is over 256 bytes or has control characters. It SHALL NOT retry the request, and SHALL NOT check the server's size limits itself.

### Requirement: change push reports the version it created or kept
Refs: D11, D12, D19, D36, D46, D48, D50, D53
`change push` SHALL exit 0 and report the stored version number with `status` `created` when the server stored a new version, or `unchanged` when the files matched the latest version and the server kept it, with that version's provenance unchanged. `replaced_version` SHALL be the version that was latest before the push, as the server reports it: for an unchanged push that is the kept version itself, and it is null only for the first version; text output SHALL name it for a created push. The `change_push` event SHALL carry the stored change under `change`, the server's push answer without its `duplicate` and `replaced_version` fields, whose `status` is the change's `proposed`, `approved` or `archived`, while the top-level `status` is the push result. When the server refuses because the stored change's latest version is archived, push SHALL exit 1 saying so and naming the id.

### Requirement: change list shows every stored change
Refs: D16, D55, G1
`goodflows change list` SHALL read every page of the project's stored changes and show each change's id, status, latest version, when it was pushed, and the numbers of the change requests that link it, read from each row's `change_requests`; a row without that field, from a server that does not send it, SHALL show no numbers and SHALL NOT fail the command. `--status` SHALL take `proposed`, `approved` or `archived` and show only changes with that status; any other value SHALL exit 2 and send nothing. Without `--status` every stored change SHALL be shown.

### Requirement: change show prints one version or one of its files
Refs: D15, D5, D26, D52
`goodflows change show <id>` SHALL show the latest version's number, status, when it was pushed, its commit, branch and dirty flag, the change requests that link the change by its id, and the version's file paths. `--version <N>` SHALL show version N instead, with the change requests read from the stored change, since the server's version read carries none, and N MUST be at least 1, else exit 2. `--file <path>` SHALL print that file's text instead of the metadata, and SHALL exit 1 naming the version's files when it has no such file. An unknown change id or version SHALL exit 1.

### Requirement: change versions lists a change's history
Refs: D28, D2, D56
`goodflows change versions <id>` SHALL list every stored version of the change, following every page, newest first, each with its number (the server's `version` field), when it was pushed, its commit, branch and dirty flag, and SHALL mark the latest.

### Requirement: change pull writes a version into the change directory
Refs: D17, D13, D14, D33, D38, D51, G2
`goodflows change pull <id>` SHALL write the latest version, or version N with `--version <N>` (N at least 1, else exit 2 and nothing is sent), to `goodflows/changes/<id>/` under the checkout's top level and SHALL NOT stage anything in git. It SHALL exit 1, write nothing and list the files when the directory holds a file whose content differs from the version's or that the version does not have, unless `--force` is given; with `--force` it SHALL write the version and delete the local files the version does not have. It SHALL exit 1 and write nothing, even with `--force`, when the stored change's latest version has status `archived`, whichever version is pulled, when the server's answer does not say the latest version's status, or when `goodflows/changes/archive/` holds a directory for the id.

### Requirement: change pull writes only change files
Refs: D22, D39, D42, D43, D54
`change pull` SHALL refuse a version, exit 1 and write nothing when any of its paths is not `change.md`, `specs/<capability>/spec.md`, `qa-scenarios.md` or `verification.md`, including any path that is absolute or has an empty, `.` or `..` segment, when a file in the version has no text, when the version lists a path more than once, or when writing would go through a symlink. Push and pull SHALL accept the same `specs/<capability>/spec.md` paths, where `<capability>` follows `goodflows change validate`'s capability name rule.

### Requirement: change pull writes all or nothing
Refs: D23, D41
When `change pull` fails or is interrupted by a signal after it started writing, with or without `--force`, `goodflows/changes/<id>/` SHALL be left exactly as it was before the pull.

### Requirement: change delete needs --yes and names the change requests that block it
Refs: D4, D18, D49
`goodflows change delete <id>` SHALL exit 2 and send nothing without `--yes`. With `--yes` it SHALL send `DELETE .../changes/<id>`; when the server refuses because change requests link the change, it SHALL exit 1 with a message naming those change requests' numbers. A `204` with no body SHALL be success.

### Requirement: push and list name a server without stored changes
Refs: D21, D31
When the server answers `change push` or `change list` with a `404`, the command SHALL send one `GET` of the project's change requests; when that answers `2xx`, it SHALL exit 1 saying the GoodFlows server does not store changes yet, and otherwise it SHALL fail with the original `404` as an ordinary server error.

### Requirement: Stored change commands follow the change request conventions
Refs: D6, D19, D20, D26, D34
`change push`, `list`, `show`, `pull`, `versions` and `delete` SHALL follow the change-requests rules for JSON output, server errors, quoted server bodies, transport failures, keeping the API key from git, and exit codes: with `--json` each prints one event of type `change_push`, `change_list`, `change_show`, `change_pull`, `change_versions` or `change_delete` with `schema_version` `1`, and each exits 0 on success, 2 for a usage or config error, 1 for any other failure, including an unknown change id, and 128 plus the signal number when interrupted by a signal. They SHALL accept `--config` as `cr` does. A `403` SHALL be the ordinary server error. `change validate`, `verified` and `archive` SHALL keep working without a config, key or request, and SHALL keep refusing `--config` with exit 2.
