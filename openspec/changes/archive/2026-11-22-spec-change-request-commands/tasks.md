# Tasks

## 1. Check the specs against the code

- [ ] 1.1 For each requirement, confirm a test in `tests/change_requests.rs` or a unit test in `src/change_requests/` or `src/output/change_requests.rs` covers it, or list it as uncovered in design.md; verify with `cargo test --locked --test change_requests` and `cargo test --locked --bin goodflows change_requests`
- [ ] 1.2 Fix the docs and recipe disagreements listed in design.md in their own commits, or file them
- [ ] 1.3 Run `npx @fission-ai/openspec validate spec-change-request-commands --strict`

## 2. Archive

- [ ] 2.1 Archive the change so `openspec/specs/` holds the nine capabilities
