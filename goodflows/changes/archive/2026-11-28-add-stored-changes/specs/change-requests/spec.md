## MODIFIED Requirements

### Requirement: Commands use the reporting project, URL and key
Refs: D27
Every `cr`, `policy`, `features` and `events` command that talks to the server, and `change push`, `change list`, `change show`, `change pull`, `change versions` and `change delete`, SHALL use the config's top-level `[reporting]` block, as `history` does: its `project`, the URL in the variable named by `url_env` and the key in the variable named by `token_env` (`GOODFLOWS_URL` and `GOODFLOWS_API_KEY` by default). Each request SHALL go to `/api/v1/projects/<project>/...` with each path segment escaped and the key sent as `Authorization: Bearer <key>`.

#### Scenario: Key and path
- **WHEN** `[reporting]` names project `goodcases`, `GOODFLOWS_API_KEY` is `sk_test` and `goodflows cr ready -n 4` runs
- **THEN** it sends `POST /api/v1/projects/goodcases/change-requests/4/ready` with header `authorization: Bearer sk_test`

#### Scenario: A branch name is escaped
- **WHEN** the current branch is `feat/login` and a command looks up its change request
- **THEN** the query string carries `source_branch=feat%2Flogin`
