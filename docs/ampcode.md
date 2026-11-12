# Using goodflows in Ampcode orbs

## Set up the orb

Commit `goodflows.toml` at the repository root. Add an executable `.agents/setup` that installs a [release archive](install.md#release-archives) into `/usr/local/bin`, plus any toolchains the jobs need. Orbs already have Git, Node.js, Python, `gh`, and `amp`.

Tell the agent to run goodflows from `AGENTS.md`:

```markdown
## Checks

Run `goodflows` from the repository root before finishing.
`goodflows --json=summary` prints JSON Lines. The final `summary` event has `success`.
Fix failures and rerun until `success` is true.
```

Ampcode snapshots the disk after setup succeeds. After you change `.agents/setup`, run `amp projects snapshots delete <project>` so the next thread installs again.

Leave `parallelism` unset so goodflows uses the orb's CPU count. The Linux release binary links against Debian 12's glibc, which the orbs have.

## Amp palette plugin

[`.amp/plugins/goodflows.ts`](../.amp/plugins/goodflows.ts) adds two palette commands, GoodFlows: Run and GoodFlows: Cancel. Run executes the default workflow, or asks you to pick one, and sends failures to the agent. Cancel stops the goodflows process.

To install it, copy the file to a project's `.amp/plugins/` or to `~/.config/amp/plugins/`, then run `plugins: reload`.
