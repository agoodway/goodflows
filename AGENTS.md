<!-- goodflows:formats -->
## File formats

This project lands work through goodflows. Before writing a scenario set, a verify run's run.md, a change
request's claims file, a feature file, or a review policy, run `goodflows schema show <format>` and follow it instead of a
skill's default format:

- Scenario sets: `qa-scenarios/2` (`--source intent` or `--source diff`)
- Verify run notes: `verify-run-notes/1`
- Change request claims: `change-claims/2`
- Feature files: `feature-file/1`
- Review policy (TOML): `review-policy/1`

After writing, run the validator the schema names and fix every error it lists.
<!-- /goodflows:formats -->
