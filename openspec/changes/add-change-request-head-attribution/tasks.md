# Tasks

## 1. Collection

- [ ] 1.1 Add a `base: Option<&str>` parameter to `links::collect` and `scan`, used instead of the `GITHUB_BASE_REF`/`origin/HEAD` lookup when set; pass `None` from `main.rs` and `sprite/mod.rs`. Add a `links.rs` test where a trailer on the base is excluded with an explicit base, and verify the existing scan tests pass unchanged with `cargo test links`.
- [ ] 1.2 Add `links::agent(lookup, session_thread, warn)` returning `Option<Agent { name, model }>` per D3, with validation per the spec. Unit-test: both variables, session only (`amp`), model without a name (none), uppercase `GOODFLOWS_AGENT` in a session (warning without the value, falls back to `amp`), invalid model (dropped, name kept). Verify with `cargo test links`.

## 2. Change request commands

- [ ] 2.1 Pass the root `[settings] ticket_prefixes` from `main.rs` into `change_requests::cr`, and add a lazily filled attribution cell to `Ctx` that calls `collect` with the change request's target ref as base. Verify `cargo build` and `cargo clippy -- -D warnings`.
- [ ] 2.2 In `report_facts`, post the document plus `links` and `agent` when present, keeping `facts` and the summary unchanged. Extend the change request integration tests (mock server) with: Amp session push body has both keys; no source sends neither key; `GOODFLOWS_AGENT`/`GOODFLOWS_MODEL` body; a trailer on `main` excluded; event output identical to before. Verify with `cargo test change_requests`.
- [ ] 2.3 In `open`, add collected `links` to the create body, and only when creating. Extend the `cr open` tests: create in an Amp session has `links`; the existing create body scenario (no sources) is unchanged; reuse runs no collection git command; open with a claims file sends the same `links` in both bodies. Verify with `cargo test change_requests`.

## 3. Documentation

- [ ] 3.1 Document `GOODFLOWS_AGENT` and `GOODFLOWS_MODEL` and what `cr open`, `cr claims` and `cr push` send in README, `src/agents_guide.md` and the bundled change request skill. Verify `cargo test` (skills and guide tests) passes.

## 4. Integration

- [ ] 4.1 Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test`, and verify all pass.
- [ ] 4.2 Against a goodflows_web build with `add-change-request-head-attribution`, run `cr open` and `cr push` in an Amp session and read the Git facts back; verify `links` and `agent` are stored.

## Workflow follow-up

- Release only after goodflows_web `add-change-request-head-attribution` is deployed, and name it as a prerequisite in the release notes.
