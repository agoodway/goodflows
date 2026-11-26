# feature-index Specification

## Purpose

Defines how `goodflows features sync` and `features validate` find the verify skill's feature files, read their front matter, and upload the files with `paths` globs as the project's feature index.

## Requirements

### Requirement: Feature commands find one features directory
`features sync` and `features validate` SHALL read `--dir` when given. Otherwise they SHALL use the one `verify-*/features` directory under `.agents/skills`, `.claude/skills` or `.cursor/skills` of the checkout's top level. With none, they SHALL fail with `found no verify-*/features directory in .agents/skills, .claude/skills or .cursor/skills; pass --dir`, and with several, `found several feature directories (<dirs>); pass --dir`.

#### Scenario: Two skills
- **WHEN** both `.agents/skills/verify-web/features` and `.claude/skills/verify-api/features` exist and a user runs `goodflows features sync` without `--dir`
- **THEN** goodflows exits 1 with a message starting `found several feature directories` and sends no request

### Requirement: Feature files are read from their front matter
Every `.md` file in the directory except `README.md` (any case) SHALL be a feature file. Its YAML front matter MAY set `id` (default: the file name without `.md`), `title` (default: the body's first `# ` heading, else the id) and `paths`, a list of glob strings. `id` MUST be a nonempty string, `title` a string, and `paths` a list of strings.

#### Scenario: Defaults
- **WHEN** `sign-in.md` has front matter `paths: ["lib/login/**", "lib/session.ex"]` and the heading `# Sign in`
- **THEN** it reads as id `sign-in`, title `Sign in`, paths `["lib/login/**", "lib/session.ex"]`

#### Scenario: Wrong types
- **WHEN** `c.md` has front matter `id: [x]` and `paths: lib/**`
- **THEN** its problems are `<file>: id must be a nonempty string` and `<file>: paths must be a list of globs`

### Requirement: The directory needs distinct ids and some paths
Among files with `paths`, no two SHALL share an id: each shared id is the problem `feature id <id> is used by <file> and <file>; give each file its own id`. When no file has `paths`, the problem SHALL be `no feature file in <dir> has paths globs in its front matter; nothing to sync`.

#### Scenario: Shared id
- **WHEN** `a.md` and `b.md` both set `id: login` and have `paths`
- **THEN** the directory has the problem `feature id login is used by <a.md> and <b.md>; give each file its own id`

### Requirement: features sync uploads the files with paths
`goodflows features sync` SHALL send `POST feature-index` with `{"features": [{"id", "title", "paths"}, ...]}` for the files with `paths`, in file name order, and skip files without `paths`. Any problem in a file or the directory SHALL fail with every problem listed and exit 1, with no request sent.

#### Scenario: One file skipped
- **WHEN** the directory has `README.md`, `sign-in.md` with paths and `api-keys.md` without
- **THEN** the body is `{"features": [{"id": "sign-in", "title": "Sign in", "paths": ["lib/login/**", "lib/session.ex"]}]}` and `api-keys` is skipped

#### Scenario: Problems stop the upload
- **WHEN** two files with paths share the id `login`
- **THEN** `features sync` exits 1, its error output contains `feature id login is used by`, and no request is sent

### Requirement: features sync prints the new index version
`features sync` SHALL print `Feature index version <v>: <n> features (<ids>)`, then `Skipped, no paths in front matter: <ids>` when files were skipped. With `--json` it SHALL print `{"type": "feature_index", "dir", "version", "features": [<ids>], "skipped": [<ids>]}`.

#### Scenario: JSON output
- **WHEN** the server answers `201` with version 4 and `api-keys` was skipped
- **THEN** the event has `version` `4` and `skipped` `["api-keys"]`

### Requirement: features validate checks without a server
`goodflows features validate` SHALL check the same files and directory rules as `features sync` without an API key, `goodflows.toml` or a request. It SHALL report each file as ok or failed with its errors, warn `<file>: no paths globs in the front matter, so features sync skips it` for a file without `paths`, and exit 1 when any file or the directory has errors, else 0.

#### Scenario: Warning only
- **WHEN** `d.md` has no `paths`, and every other file is valid with `paths` and a distinct id
- **THEN** `features validate` exits 0, `d.md` is ok with that warning, and `skipped` is `["d"]`

#### Scenario: Errors exit 1
- **WHEN** `c.md` has `id: [x]`
- **THEN** `features validate` exits 1 and `c.md` is reported failed

### Requirement: features validate prints a summary
The text output SHALL print `ok    <file>` or `failed <file>` per file, each error as `  error: ...` and warning as `  warning: ...`, directory errors as `error: ...`, and `<n> checked: <ok> ok, <failed> failed`, adding `; the files together have errors` when the directory has errors. With `--json` it SHALL print `{"type": "feature_validation", "dir", "ok", "features", "skipped", "files", "errors"}`.

#### Scenario: Shared id summary
- **WHEN** four files are checked, one failed, and two files share an id
- **THEN** the last line is `4 checked: 3 ok, 1 failed; the files together have errors`
