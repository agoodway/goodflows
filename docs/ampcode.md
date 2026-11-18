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

## Warm caches in setup

Jobs that declare [caches](../README.md#caches) can warm them from `.agents/setup`, after it installs goodflows, the toolchains and dependencies:

```toml
[[workflows.orb-setup.jobs]]
name = "plt"
command = "mix dialyzer --plt"

[[workflows.orb-setup.jobs.caches]]
name = "plts"
paths = ["priv/plts"]
key.files = [".tool-versions", "mix.lock"]
```

```bash
goodflows run orb-setup
```

When the main workflow's job declares the same cache, both share entries. The first orb restores the PLT from the remote store when CI already built it. A later setup over a snapshot finds the PLT present and keeps it.

Give orbs a long-lived read-only `ck_` key bound to the default branch. Mint it with an `sk_` key:

```bash
curl -sS -X POST "$GOODFLOWS_URL/api/v1/projects/<project>/cache-keys" \
  -H "Authorization: Bearer $GOODFLOWS_API_KEY" -H "Content-Type: application/json" \
  -d '{"access":"read","ttl_seconds":null,"scope":"main","name":"orbs"}'
```

Store the returned token as the Amp project secret `GOODFLOWS_CACHE_KEY`, and let CI on the default branch write the caches. Setup receives project secrets and snapshots are shared with project members, so a writable key in an orb would let any thread's experimental code save into the default branch's caches. An orb on a feature branch still reads the default branch's entries.

The local store also ends up in the snapshot, which keeps each cache twice. Skip it in orbs with a profile, and run `goodflows run orb-setup --env orb`:

```toml
[environments.orb.cache]
local = false
```

Leave `parallelism` unset so goodflows uses the orb's CPU count. The Linux release binary links against Debian 12's glibc, which the orbs have.

## Amp palette plugin

[`.amp/plugins/goodflows.ts`](../.amp/plugins/goodflows.ts) adds two palette commands, GoodFlows: Run and GoodFlows: Cancel. Run executes the default workflow, or asks you to pick one, and sends failures to the agent. Cancel stops the goodflows process.

To install it, copy the file to a project's `.amp/plugins/` or to `~/.config/amp/plugins/`, then run `plugins: reload`.
