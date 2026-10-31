import assert from 'node:assert/strict'
import { execFile } from 'node:child_process'
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { test } from 'node:test'
import { promisify } from 'node:util'
import install from '../.amp/plugins/goodflows.ts'

const exec = promisify(execFile)
const binary = resolve('target/debug/goodflows')

// Only Amp's host is simulated: config discovery and GoodFlows execution are real.
async function host(t, config = '[[jobs]]\nname = "check"\ncommand = "true"\n', options = {}) {
  const cwd = await mkdtemp(join(tmpdir(), "goodflows amp ' "))
  t.after(() => rm(cwd, { recursive: true, force: true }))
  if (config !== null) await writeFile(join(cwd, options.hidden ? '.goodflows.toml' : 'goodflows.toml'), config)
  const commands = new Map()
  const notifications = []
  const messages = []
  const choices = []
  let dispose
  install({
    registerCommand(id, metadata, handler) { commands.set(id, { metadata, handler }) },
    on() { assert.fail('The plugin must not register lifecycle hooks') },
    onDispose(callback) { dispose = callback },
  })
  t.after(() => dispose?.())
  const ctx = {
    $: async () => ({ exitCode: 0, stdout: `${cwd}\n`, stderr: '' }),
    ui: {
      notify: async (message) => { notifications.push(message) },
      select: async (dialog) => { choices.push(dialog); return options.selection },
    },
    thread: { id: 'original', append: async (items) => { messages.push(...items) } },
  }
  const originalPath = process.env.PATH
  const binDir = join(cwd, 'bin')
  const { mkdir, symlink } = await import('node:fs/promises')
  await mkdir(binDir)
  if (options.fake) {
    await writeFile(join(binDir, 'goodflows'), `#!/bin/sh\n${options.fake}\n`, { mode: 0o755 })
  } else if (!options.missingBinary) {
    await symlink(binary, join(binDir, 'goodflows'))
  }
  process.env.PATH = options.missingBinary ? binDir : `${binDir}:${originalPath}`
  t.after(() => { process.env.PATH = originalPath })
  return {
    cwd, ctx, commands, notifications, messages, choices,
    run: () => commands.get('goodflows-run').handler(ctx),
    cancel: () => commands.get('goodflows-cancel').handler(ctx),
  }
}

test('registers an explicit command without running anything at load', async (t) => {
  const h = await host(t)
  assert.deepEqual(h.commands.get('goodflows-run').metadata, {
    title: 'Run', category: 'GoodFlows', description: 'Run project checks and send failures to the agent to fix.',
  })
  assert.equal(h.messages.length, 0)
  assert.equal(h.notifications.length, 0)
})

test('runs default workflow and appends success to the invoking thread', async (t) => {
  const h = await host(t)
  await h.run()
  assert.equal(h.messages.length, 1)
  assert.equal(h.messages[0].type, 'user-message')
  assert.match(h.messages[0].content, /passed/i)
  assert.match(h.messages[0].content, /default/)
  assert.equal(h.choices.length, 0)
})

test('hands the agent failing commands, exit codes, tails, and a literal rerun command', async (t) => {
  const h = await host(t, '[[jobs]]\nname = "lint"\ncommand = "echo lint-detail >&2; exit 7"\n')
  await h.run()
  const message = h.messages[0].content
  assert.match(message, /lint-detail/)
  assert.match(message, /lint/)
  assert.match(message, /exit_code.*7/)
  assert.match(message, /fix/i)
  assert.match(message, /rerun/i)
  assert.match(message, /diagnostic data/i)
  const rerun = message.split('\n').find((line) => line.startsWith('cd '))
  assert.ok(rerun)
  const result = await exec('/bin/sh', ['-c', `${rerun} > rerun.json; true`], { cwd: h.cwd })
  assert.equal(result.stderr, '')
  assert.match(await readFile(join(h.cwd, 'rerun.json'), 'utf8'), /"type":"summary"/)
})

test('supports hidden config but prefers goodflows.toml when both exist', async (t) => {
  const h = await host(t, undefined, { hidden: true })
  await h.run()
  assert.match(h.messages[0].content, /\.goodflows\.toml/)
  await writeFile(join(h.cwd, 'goodflows.toml'), '[[jobs]]\nname = "preferred"\ncommand = "exit 1"\n')
  await h.run()
  assert.match(h.messages[1].content, /preferred/)
})

test('requires an active thread before running checks', async (t) => {
  const h = await host(t, '[[jobs]]\nname = "check"\ncommand = "touch ran"\n')
  h.ctx.thread = undefined
  await h.run()
  assert.match(h.notifications.join('\n'), /active thread/i)
  assert.equal(h.messages.length, 0)
  await assert.rejects(readFile(join(h.cwd, 'ran')), { code: 'ENOENT' })
})

test('missing configuration does not initialize the project', async (t) => {
  const h = await host(t, null)
  await h.run()
  assert.match(h.notifications.join('\n'), /no.*config/i)
  await assert.rejects(readFile(join(h.cwd, 'goodflows.toml')), { code: 'ENOENT' })
})

test('missing executable is handed to the agent as a blocker', async (t) => {
  const h = await host(t, undefined, { missingBinary: true })
  await h.run()
  assert.match(h.messages[0].content, /ENOENT|not found|not installed/i)
  assert.doesNotMatch(h.messages[0].content, /checks passed/i)
})

test('selects a named workflow without interpreting shell punctuation', async (t) => {
  const workflow = 'ci; touch INJECTED'
  const h = await host(t, `[[workflows."${workflow}".jobs]]\nname = "named-check"\ncommand = "echo named >&2; exit 2"\n`, { selection: workflow })
  await h.run()
  assert.deepEqual(h.choices[0].options, [workflow])
  assert.match(h.messages[0].content, /named-check/)
  await assert.rejects(readFile(join(h.cwd, 'INJECTED')), { code: 'ENOENT' })
})

test('cancelling the workflow picker never executes jobs or sends a repair task', async (t) => {
  const h = await host(t, '[[workflows.ci.jobs]]\nname = "check"\ncommand = "touch ran"\n')
  await h.run()
  assert.equal(h.messages.length, 0)
  await assert.rejects(readFile(join(h.cwd, 'ran')), { code: 'ENOENT' })
})

test('workflow names starting with a dash remain positional arguments', async (t) => {
  const h = await host(t, '[[workflows."--ci".jobs]]\nname = "dash-workflow"\ncommand = "true"\n', { selection: '--ci' })
  await h.run()
  assert.match(h.messages[0].content, /checks passed/i)
})

test('a directory named goodflows.toml is reported as an error instead of falling back', async (t) => {
  const h = await host(t, undefined, { hidden: true })
  const { mkdir } = await import('node:fs/promises')
  await mkdir(join(h.cwd, 'goodflows.toml'))
  await h.run()
  assert.doesNotMatch(h.messages[0].content, /checks passed/i)
  assert.match(h.messages[0].content, /directory/i)
})

test('invalid configuration is passed to the agent for repair', async (t) => {
  const h = await host(t, 'invalid toml')
  await h.run()
  assert.match(h.messages[0].content, /parse|TOML|expected/i)
  assert.match(h.messages[0].content, /fix/i)
})

for (const [name, output, exit] of [
  ['missing summary', '{"schema_version":1,"type":"run_started"}', 0],
  ['invalid JSON', 'not JSON', 0],
  ['unsupported schema', '{"schema_version":99,"type":"summary","success":true,"results":[]}', 0],
  ['nonzero exit despite passing summary', '{"schema_version":1,"type":"summary","success":true,"results":[]}', 3],
]) {
  test(`${name} never reports passing checks`, async (t) => {
    const discovery = '{"schema_version":1,"type":"workflows","workflows":[{"name":"default","jobs":1,"settings":{}}],"default_workflow":"default"}'
    const h = await host(t, undefined, { fake: `case "$1" in list) printf '%s\\n' '${discovery}';; *) printf '%s\\n' '${output}'; exit ${exit};; esac` })
    await h.run()
    assert.equal(h.messages.length, 1)
    assert.doesNotMatch(h.messages[0].content, /checks passed/i)
    assert.match(h.messages[0].content, /fix|investigate/i)
  })
}

test('captures the invoking thread across a switch and rejects overlapping runs', async (t) => {
  const h = await host(t)
  let release
  const ready = new Promise((resolve) => { release = resolve })
  h.ctx.$ = async () => { await ready; return { exitCode: 0, stdout: `${h.cwd}\n`, stderr: '' } }
  const first = h.run()
  h.ctx.thread = { id: 'other', append: async () => assert.fail('Results sent to wrong thread') }
  await h.run()
  assert.match(h.notifications.join('\n'), /already running/i)
  release()
  await first
  assert.equal(h.messages.length, 1)
})

test('bounds diagnostic handoff even with noisy failing jobs', async (t) => {
  const h = await host(t, '[[jobs]]\nname = "noisy"\ncommand = "yes diagnostic | head -c 200000; echo final-detail >&2; exit 1"\n')
  await h.run()
  assert.match(h.messages[0].content, /final-detail/)
  assert.ok(h.messages[0].content.length < 70000)
})

test('preserves metadata and a log excerpt for later failures when many jobs fail', async (t) => {
  const jobs = Array.from({ length: 12 }, (_, index) => `[[jobs]]\nname = "failure_${index}"\ncommand = "yes stdout | head -c 5000; yes stderr | head -c 5000 >&2; echo final_${index} >&2; exit 7"\n`).join('\n')
  const h = await host(t, `[settings]\nfail_fast = false\nparallelism = 4\n${jobs}`)
  await h.run()
  const message = h.messages[0].content
  assert.ok(message.length < 70000)
  for (let index = 0; index < 12; index++) {
    assert.match(message, new RegExp(`"name": "failure_${index}"`))
    assert.match(message, new RegExp(`final_${index}`))
  }
})

test('cancel terminates a running check and does not start repairs', async (t) => {
  const h = await host(t, '[[jobs]]\nname = "slow"\ncommand = "touch started; sleep 30; touch finished"\n')
  const run = h.run()
  for (let n = 0; n < 200; n++) {
    try { await readFile(join(h.cwd, 'started')); break } catch { await new Promise((resolve) => setTimeout(resolve, 10)) }
  }
  assert.equal(await readFile(join(h.cwd, 'started'), 'utf8'), '')
  await h.cancel()
  await run
  assert.equal(h.messages.length, 0)
  assert.match(h.notifications.join('\n'), /cancel/i)
  await assert.rejects(readFile(join(h.cwd, 'finished')), { code: 'ENOENT' })
})
