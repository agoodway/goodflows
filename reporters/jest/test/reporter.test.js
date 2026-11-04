'use strict';

// Drives the reporter's hooks with payloads recorded from a real Jest run of
// reporters/fixtures/jest (test/payloads.json, with paths rooted at /fixture),
// and checks the written reports against the test-timings/1 key sets.

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');

const Reporter = require('..');

const RECORDED = JSON.parse(fs.readFileSync(path.join(__dirname, 'payloads.json'), 'utf8'));

const COMMON = ['event', 'suite_id', 'report_id', 'received_offset_us'];
const KEYS = {
  suite_started: ['format', 'started_at', 'runner', 'partition', 'partition_count', 'seed', 'filters'],
  group_started: ['group', 'file', 'execution_id', 'started_offset_us'],
  group_finished: ['group', 'file', 'execution_id', 'started_offset_us', 'duration_us', 'outcome', 'counts'],
  test_started: ['group', 'path', 'name', 'file', 'line', 'execution_id', 'started_offset_us'],
  test_finished: [
    'group', 'path', 'name', 'file', 'line', 'execution_id', 'started_offset_us', 'duration_us', 'outcome',
  ],
  suite_finished: ['aborted', 'unfinished_tests', 'unfinished_groups', 'counts', 'duration_us'],
  interrupted: [],
  max_failures_reached: [],
};
const PAIRS = { test_finished: 'test_started', group_finished: 'group_started' };
const ENV = [
  'GOODFLOWS_REPORT_DIR', 'GOODFLOWS_PARTITION', 'GOODFLOWS_PARTITION_COUNT', 'TEST_TIMING_SUITE_ID',
  'GOODFLOWS_RUN_ID',
];

function tmp() {
  return fs.mkdtempSync(path.join(os.tmpdir(), 'goodflows-jest-'));
}

// Recorded payloads, rerooted from /fixture to `root`.
function recorded(root) {
  return JSON.parse(JSON.stringify(RECORDED).split('/fixture').join(root));
}

function withEnv(env, fn) {
  const saved = Object.fromEntries(ENV.map((name) => [name, process.env[name]]));
  for (const name of ENV) delete process.env[name];
  Object.assign(process.env, env);
  try {
    return fn();
  } finally {
    for (const name of ENV) {
      if (saved[name] === undefined) delete process.env[name];
      else process.env[name] = saved[name];
    }
  }
}

function captureStderr(fn) {
  const original = process.stderr.write;
  const lines = [];
  process.stderr.write = (chunk) => {
    lines.push(String(chunk));
    return true;
  };
  try {
    fn();
  } finally {
    process.stderr.write = original;
  }
  return lines;
}

// Replays the recorded run, letting `edit` change each call's arguments.
function replay(reporter, root, edit = (call) => call) {
  for (const [hook, ...args] of recorded(root).calls) {
    const edited = edit([hook, ...args]);
    if (edited) reporter[edited[0]](...edited.slice(1));
  }
}

function run({ env = {}, options = {}, global = {}, edit } = {}) {
  const root = tmp();
  const dir = path.join(root, 'reports');
  return withEnv({ GOODFLOWS_REPORT_DIR: dir, ...env }, () => {
    const globalConfig = { ...recorded(root).globalConfig, ...global };
    const reporter = new Reporter(globalConfig, options);
    replay(reporter, root, edit);
    return { root, dir, records: readReports(options.outputDirectory ? path.resolve(root, options.outputDirectory) : dir) };
  });
}

function readReports(dir) {
  const files = fs.readdirSync(dir).filter((f) => f.endsWith('.jsonl'));
  assert.equal(files.length, 1, `one report in ${dir}`);
  return readRecords(path.join(dir, files[0]));
}

function readRecords(file) {
  const text = fs.readFileSync(file, 'utf8');
  assert.ok(text === '' || text.endsWith('\n'), 'every record is a complete line');
  return text.split('\n').filter(Boolean).map((line) => JSON.parse(line));
}

// Every record has exactly its event's keys (plus optional meta), identities
// agree, and each finished event repeats its open start's core values.
function assertContract(records) {
  const open = new Map();
  for (const record of records) {
    const keys = Object.keys(record).filter((k) => k !== 'meta').sort();
    assert.deepEqual(keys, [...COMMON, ...KEYS[record.event]].sort(), record.event);
    assert.equal(record.suite_id, records[0].suite_id);
    assert.equal(record.report_id, records[0].report_id);
    if (record.meta !== undefined) {
      for (const [key, value] of Object.entries(record.meta)) {
        assert.match(key, /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/);
        assert.ok(value === null || ['boolean', 'string'].includes(typeof value) || Number.isSafeInteger(value));
      }
    }
    if (record.event === 'test_started' || record.event === 'group_started') {
      const key = `${record.event}:${record.execution_id}`;
      assert.ok(!open.has(key), `duplicate ${key}`);
      open.set(key, record);
    } else if (PAIRS[record.event]) {
      const key = `${PAIRS[record.event]}:${record.execution_id}`;
      const started = open.get(key);
      assert.ok(started, `unpaired ${record.event}`);
      open.delete(key);
      for (const field of KEYS[PAIRS[record.event]]) {
        assert.deepEqual(record[field], started[field], field);
      }
    }
  }
  const last = records[records.length - 1];
  if (last.event === 'suite_finished') {
    const count = (kind) => [...open.keys()].filter((k) => k.startsWith(kind)).length;
    assert.equal(last.unfinished_tests, count('test_started'));
    assert.equal(last.unfinished_groups, count('group_started'));
  }
}

// Drives the hooks directly with synthetic payloads; `drive` gets the reporter.
function synthetic(drive, { global = {}, options = {}, env = {} } = {}) {
  const root = tmp();
  const dir = path.join(root, 'reports');
  withEnv({ GOODFLOWS_REPORT_DIR: dir, ...env }, () => {
    const reporter = new Reporter({ rootDir: root, maxWorkers: 1, bail: 0, ...global }, options);
    reporter.onRunStart({ startTime: Date.now(), numFailedTests: 0, numFailedTestSuites: 0 });
    drive(reporter, root);
  });
  return readReports(dir);
}

const testFile = (root, name, config = {}) => ({
  path: path.join(root, '__tests__', name),
  context: { config: { rootDir: root, ...config } },
});
const caseResult = (title, status, extra = {}) => ({
  ancestorTitles: [], title, status, duration: 2, invocations: 1, ...extra,
});

const finished = (records, name) =>
  records.find((r) => r.event === 'test_finished' && r.name === name);
const group = (records, file) =>
  records.find((r) => r.event === 'group_finished' && r.group === file);

test('a recorded Jest run maps onto the core model', () => {
  const { records } = run();
  assertContract(records);
  const [started] = records;
  assert.equal(started.format, 'test-timings/1');
  assert.deepEqual(started.runner, { name: 'jest', version: null });
  assert.equal(started.filters, null);
  assert.equal(started.seed, RECORDED.globalConfig.seed);
  assert.equal(started.meta.duration_resolution_us, 1000);
  assert.equal(started.meta.node_version, process.version);
  assert.equal(started.meta.max_workers, RECORDED.globalConfig.maxWorkers);
  assert.ok(!Number.isNaN(Date.parse(started.started_at)));

  const adds = finished(records, 'adds');
  assert.deepEqual(adds.path, ['math', 'sum']);
  assert.equal(adds.group, '__tests__/nested.test.js');
  assert.equal(adds.file, '__tests__/nested.test.js');
  assert.equal(adds.outcome, 'passed');
  assert.equal(finished(records, 'multiplies').path.length, 1);
  assert.deepEqual(finished(records, 'passes').path, []);

  // Durations are Jest's milliseconds times 1,000.
  const recordedCase = RECORDED.calls.find((c) => c[0] === 'onTestCaseResult' && c[2].title === 'adds')[2];
  assert.equal(adds.duration_us, recordedCase.duration * 1000);

  const groupStarted = records.filter((r) => r.event === 'group_started');
  assert.ok(groupStarted.every((r) => r.meta.project === 'fixture'));
  const nested = group(records, '__tests__/nested.test.js');
  assert.equal(nested.outcome, 'passed');
  assert.deepEqual(nested.counts, { passed: 3 });
  for (const key of ['load_test_environment_us', 'setup_files_us', 'slow']) {
    assert.ok(key in nested.meta, key);
  }

  const summary = records[records.length - 1];
  assert.equal(summary.event, 'suite_finished');
  assert.equal(summary.aborted, false);
  assert.deepEqual(summary.counts, { passed: 4, failed: 1, skipped: 3 });
});

test('a todo test is skipped with no duration', () => {
  const todo = finished(run().records, 'is a todo');
  assert.equal(todo.outcome, 'skipped');
  assert.equal(todo.duration_us, null);
  assert.equal(todo.meta.skip_reason, 'todo');
});

test('skipped statuses keep their Jest status as the reason', () => {
  const { records } = run();
  for (const name of ['is skipped', 'is filtered out by name']) {
    const record = finished(records, name);
    assert.equal(record.outcome, 'skipped');
    assert.equal(record.meta.skip_reason, 'pending');
  }
});

test('a result delivered without a start writes the start first', () => {
  const { records } = run();
  const index = records.findIndex((r) => r.event === 'test_finished' && r.name === 'is a todo');
  assert.equal(records[index - 1].event, 'test_started');
  assert.equal(records[index - 1].execution_id, records[index].execution_id);
});

test('a file with a failed test fails, and a file that cannot load errors', () => {
  const { records } = run();
  assert.equal(finished(records, 'fails on purpose').outcome, 'failed');
  assert.equal(group(records, '__tests__/outcomes.test.js').outcome, 'failed');
  const errored = group(records, '__tests__/load-error.test.js');
  assert.equal(errored.outcome, 'errored');
  assert.deepEqual(errored.counts, {});
});

test('a failing assertion message never reaches the report', () => {
  const { dir } = run();
  const [file] = fs.readdirSync(dir);
  const text = fs.readFileSync(path.join(dir, file), 'utf8');
  assert.ok(!text.includes('private assertion value'));
  assert.ok(!text.includes('fixture file fails to load'));
});

test('the line is known only from the result location', () => {
  const { records } = run({
    edit: ([hook, ...args]) => {
      if (hook === 'onTestCaseResult' && args[1].title === 'adds') {
        args[1] = { ...args[1], location: { line: 40, column: 5 }, invocations: 3 };
      }
      return [hook, ...args];
    },
  });
  assertContract(records);
  const start = records.find((r) => r.event === 'test_started' && r.name === 'adds');
  const adds = finished(records, 'adds');
  assert.equal(start.line, null);
  assert.equal(adds.line, null);
  assert.equal(adds.meta.line, 40);
  assert.equal(adds.meta.invocations, 3);
  assert.equal(finished(records, 'passes').meta, undefined);
});

test('a retried test records its invocations in meta', () => {
  // A test retried once by jest.retryTimes reports invocations: 2.
  const { records } = run({
    edit: ([hook, ...args]) => {
      if (hook === 'onTestCaseResult' && args[1].title === 'fails on purpose') {
        args[1] = { ...args[1], invocations: 2 };
      }
      return [hook, ...args];
    },
  });
  assertContract(records);
  const retried = finished(records, 'fails on purpose');
  assert.equal(retried.meta.invocations, 2);
  assert.equal(retried.outcome, 'failed');
  // A single invocation (the recorded value) is not written.
  const once = RECORDED.calls.find((c) => c[0] === 'onTestCaseResult' && c[2].title === 'passes')[2];
  assert.equal(once.invocations, 1);
  assert.equal(finished(records, 'passes').meta, undefined);
  for (const record of records.filter((r) => r.event === 'test_finished' && r.name !== 'fails on purpose')) {
    assert.ok(!record.meta || !('invocations' in record.meta), record.name);
  }
});

test('multi-project files are grouped relative to the global root directory', () => {
  const root = tmp();
  const dir = path.join(root, 'reports');
  const project = (name) => ({
    path: path.join(root, 'packages', name, '__tests__', 'same.test.js'),
    context: { config: { rootDir: path.join(root, 'packages', name), displayName: { name } } },
  });
  const result = (title) => ({ ancestorTitles: [], title, status: 'passed', duration: 2, invocations: 1 });
  const outside = { path: path.join(os.tmpdir(), 'elsewhere', 'outside.test.js'), context: { config: {} } };
  withEnv({ GOODFLOWS_REPORT_DIR: dir }, () => {
    const reporter = new Reporter({ rootDir: root, maxWorkers: 1 }, {});
    reporter.onRunStart({ startTime: Date.now() });
    for (const test of [project('a'), project('b'), outside]) {
      reporter.onTestFileStart(test);
      reporter.onTestCaseStart(test, { ancestorTitles: [], title: 'works', startedAt: Date.now() });
      reporter.onTestCaseResult(test, result('works'));
      reporter.onTestFileResult(test, { testResults: [result('works')], perfStats: {} }, {});
    }
    reporter.onRunComplete(new Set(), {});
  });
  const records = readReports(dir);
  assertContract(records);
  const groups = records.filter((r) => r.event === 'group_finished');
  assert.deepEqual(
    groups.map((r) => [r.group, r.file, r.meta.project]),
    [
      ['packages/a/__tests__/same.test.js', 'packages/a/__tests__/same.test.js', 'a'],
      ['packages/b/__tests__/same.test.js', 'packages/b/__tests__/same.test.js', 'b'],
      ['outside.test.js', null, undefined],
    ],
  );
  const tests = records.filter((r) => r.event === 'test_finished');
  assert.deepEqual(tests.map((r) => r.group), groups.map((r) => r.group));
});

test('an interrupted run writes interrupted before suite_finished', () => {
  const { records } = run({
    edit: ([hook, ...args]) => {
      if (hook === 'onRunComplete') args[1] = { ...args[1], wasInterrupted: true };
      return [hook, ...args];
    },
  });
  assertContract(records);
  const events = records.map((r) => r.event);
  assert.deepEqual(events.slice(-2), ['interrupted', 'suite_finished']);
  assert.equal(records[records.length - 1].aborted, true);
});

test('bail writes max_failures_reached once, before suite_finished', () => {
  const { records } = run({ global: { bail: 1 } });
  assertContract(records);
  const events = records.map((r) => r.event);
  assert.equal(events.filter((e) => e === 'max_failures_reached').length, 1);
  assert.ok(events.indexOf('max_failures_reached') < events.indexOf('suite_finished'));
  assert.equal(records[records.length - 1].aborted, true);
});

test('a run killed midway leaves a valid prefix without suite_finished', () => {
  const root = tmp();
  const dir = path.join(root, 'reports');
  withEnv({ GOODFLOWS_REPORT_DIR: dir }, () => {
    const reporter = new Reporter(recorded(root).globalConfig, {});
    const calls = recorded(root).calls;
    for (const [hook, ...args] of calls.slice(0, calls.length - 1)) reporter[hook](...args);
  });
  const records = readReports(dir);
  assertContract(records);
  assert.ok(records.every((r) => r.event !== 'suite_finished'));
  assert.equal(records.filter((r) => r.event === 'test_finished').length, 8);
});

test('partitions come from goodflows, then Jest shards, then none', () => {
  const shard = { shard: { shardIndex: 2, shardCount: 3 } };
  const cases = [
    [{}, {}, null, 1],
    [{}, shard, '2', 3],
    [{ GOODFLOWS_PARTITION: '1', GOODFLOWS_PARTITION_COUNT: '4' }, shard, '1', 4],
    [{ GOODFLOWS_PARTITION: 'a b' }, shard, '2', 3],
    // Both goodflows variables must be valid; otherwise neither is used.
    [{ GOODFLOWS_PARTITION: '1' }, shard, '2', 3],
    [{ GOODFLOWS_PARTITION: '1', GOODFLOWS_PARTITION_COUNT: '0' }, shard, '2', 3],
    [{ GOODFLOWS_PARTITION: '1', GOODFLOWS_PARTITION_COUNT: 'x' }, {}, null, 1],
    [{ GOODFLOWS_PARTITION_COUNT: '4' }, {}, null, 1],
  ];
  for (const [env, global, partition, count] of cases) {
    const [started] = run({ env, global }).records;
    assert.deepEqual([started.partition, started.partition_count], [partition, count]);
  }
});

test('shards share a suite id and keep distinct report ids', () => {
  const ids = [1, 2, 3].map((index) => {
    const [started] = run({
      env: { TEST_TIMING_SUITE_ID: 'nightly-42' },
      global: { shard: { shardIndex: index, shardCount: 3 } },
    }).records;
    return started;
  });
  assert.ok(ids.every((r) => r.suite_id === 'nightly-42'));
  assert.equal(new Set(ids.map((r) => r.report_id)).size, 3);
  // The option wins, and an invalid identity falls back to the report id.
  const [option] = run({ env: { TEST_TIMING_SUITE_ID: 'nightly-42' }, options: { suiteId: 'mine' } }).records;
  assert.equal(option.suite_id, 'mine');
  const [invalid] = run({ env: { TEST_TIMING_SUITE_ID: 'a/b' } }).records;
  assert.equal(invalid.suite_id, invalid.report_id);
});

test('the directory is the option, then GOODFLOWS_REPORT_DIR, then the default', () => {
  const explicit = run({ options: { outputDirectory: 'custom' } });
  assert.ok(fs.existsSync(path.join(explicit.root, 'custom')));
  assert.ok(!fs.existsSync(explicit.dir));
  const root = tmp();
  withEnv({ GOODFLOWS_REPORT_DIR: '' }, () => {
    replay(new Reporter(recorded(root).globalConfig, {}), root);
  });
  assertContract(readReports(path.join(root, '.goodflows', 'test-timings')));
});

test('watch mode writes each run to a new report', () => {
  const root = tmp();
  const dir = path.join(root, 'reports');
  withEnv({ GOODFLOWS_REPORT_DIR: dir }, () => {
    const reporter = new Reporter(recorded(root).globalConfig, {});
    replay(reporter, root);
    replay(reporter, root);
  });
  const files = fs.readdirSync(dir);
  assert.equal(files.length, 2);
  const ids = files.map((f) => readRecords(path.join(dir, f))[0].report_id);
  assert.notEqual(ids[0], ids[1]);
  files.forEach((f) => assertContract(readRecords(path.join(dir, f))));
});

test('long names are cut to 4,096 bytes on a character boundary', () => {
  const long = '€'.repeat(2000);
  const { records } = run({
    edit: ([hook, ...args]) => {
      if ((hook === 'onTestCaseStart' || hook === 'onTestCaseResult') && args[1].title === 'passes') {
        args[1] = { ...args[1], title: long };
      }
      if (hook === 'onTestFileResult') {
        args[1] = {
          ...args[1],
          testResults: args[1].testResults.map((r) => (r.title === 'passes' ? { ...r, title: long } : r)),
        };
      }
      return [hook, ...args];
    },
  });
  assertContract(records);
  const record = records.find((r) => r.event === 'test_finished' && r.name.startsWith('€'));
  assert.ok(Buffer.byteLength(record.name) <= 4096);
  assert.equal(record.name, '€'.repeat(1365));
});

test('an unwritable directory warns once and never throws', () => {
  const root = tmp();
  const blocker = path.join(root, 'blocker');
  fs.writeFileSync(blocker, '');
  const lines = captureStderr(() => {
    withEnv({ GOODFLOWS_REPORT_DIR: path.join(blocker, 'reports') }, () => {
      const reporter = new Reporter(recorded(root).globalConfig, {});
      assert.doesNotThrow(() => replay(reporter, root));
    });
  });
  assert.equal(lines.filter((l) => l.includes('goodflows-jest-reporter stopped writing')).length, 1);
  assert.ok(!fs.existsSync(path.join(blocker, 'reports')));
});

test('hooks swallow unexpected payloads', () => {
  const root = tmp();
  const lines = captureStderr(() => {
    withEnv({ GOODFLOWS_REPORT_DIR: path.join(root, 'reports') }, () => {
      const reporter = new Reporter(recorded(root).globalConfig, {});
      reporter.onRunStart({});
      assert.doesNotThrow(() => reporter.onTestFileResult(null, { testResults: [null] }, null));
      assert.doesNotThrow(() => reporter.onRunComplete(null, null));
    });
  });
  assert.equal(lines.length, 1);
});

test('bail counts failed tests, not failed files, as Jest does', () => {
  // Two failures in one file reach bail: 2 after that file.
  const twoFailures = synthetic((reporter, root) => {
    const file = testFile(root, 'two.test.js');
    reporter.onTestFileStart(file);
    const results = [caseResult('one', 'failed'), caseResult('two', 'failed')];
    results.forEach((r) => reporter.onTestCaseResult(file, r));
    reporter.onTestFileResult(file, { testResults: results, perfStats: {} }, {
      numFailedTests: 2, numFailedTestSuites: 1,
    });
    reporter.onRunComplete(new Set(), { numFailedTests: 2, numFailedTestSuites: 1 });
  }, { global: { bail: 2 } });
  assertContract(twoFailures);
  const events = twoFailures.map((r) => r.event);
  assert.deepEqual(events.slice(-3), ['group_finished', 'max_failures_reached', 'suite_finished']);
  assert.equal(twoFailures[twoFailures.length - 1].aborted, true);

  // A file that fails to load fails a suite but no test, so bail: 1 does not stop Jest.
  const loadError = synthetic((reporter, root) => {
    const file = testFile(root, 'load.test.js');
    reporter.onTestFileStart(file);
    reporter.onTestFileResult(file, { testResults: [], testExecError: { message: 'boom' }, perfStats: {} }, {
      numFailedTests: 0, numFailedTestSuites: 1,
    });
    reporter.onRunComplete(new Set(), { numFailedTests: 0, numFailedTestSuites: 1 });
  }, { global: { bail: 1 } });
  assertContract(loadError);
  assert.ok(loadError.every((r) => r.event !== 'max_failures_reached'));
  assert.equal(group(loadError, '__tests__/load.test.js').outcome, 'errored');
  assert.equal(loadError[loadError.length - 1].aborted, false);
});

test('a retried test writes every attempt and counts only the final one', () => {
  // As jest.retryTimes(1) delivers it: a start and a result per attempt.
  const records = synthetic((reporter, root) => {
    const file = testFile(root, 'flaky.test.js');
    reporter.onTestFileStart(file);
    for (const [status, invocations] of [['failed', 1], ['passed', 2]]) {
      reporter.onTestCaseStart(file, { ancestorTitles: [], title: 'flaky', startedAt: Date.now() });
      reporter.onTestCaseResult(file, caseResult('flaky', status, { invocations }));
    }
    reporter.onTestCaseStart(file, { ancestorTitles: [], title: 'steady', startedAt: Date.now() });
    reporter.onTestCaseResult(file, caseResult('steady', 'passed'));
    const final = [caseResult('flaky', 'passed', { invocations: 2 }), caseResult('steady', 'passed')];
    reporter.onTestFileResult(file, { testResults: final, perfStats: {} }, {});
    reporter.onRunComplete(new Set(), {});
  });
  assertContract(records);
  const attempts = records.filter((r) => r.event === 'test_finished' && r.name === 'flaky');
  assert.deepEqual(attempts.map((r) => r.outcome), ['failed', 'passed']);
  assert.equal(attempts[0].meta, undefined);
  assert.equal(attempts[1].meta.invocations, 2);
  assert.notEqual(attempts[0].execution_id, attempts[1].execution_id);
  assert.deepEqual(group(records, '__tests__/flaky.test.js').counts, { passed: 2 });
  const summary = records[records.length - 1];
  assert.deepEqual(summary.counts, { passed: 2 });
  // The file's own results were all delivered, so none is written twice.
  assert.equal(records.filter((r) => r.event === 'test_finished').length, 3);
});

test('the same file in two projects stays two groups, even interleaved', () => {
  // As Jest delivers it: file events carry each project's context, but case
  // events carry the context of whichever project created the test runner.
  const records = synthetic((reporter, root) => {
    const a = testFile(root, 'same.test.js', { id: 'project-a', displayName: { name: 'a' } });
    const b = testFile(root, 'same.test.js', { id: 'project-b', displayName: { name: 'b' } });
    const cases = a;
    const flaky = { ancestorTitles: [], title: 'flaky', startedAt: Date.now() };
    reporter.onTestFileStart(a);
    reporter.onTestFileStart(b);
    reporter.onTestCaseStart(cases, flaky);
    reporter.onTestCaseStart(cases, flaky);
    reporter.onTestCaseResult(cases, caseResult('flaky', 'failed'));
    reporter.onTestCaseResult(cases, caseResult('flaky', 'failed'));
    // Project b's retry passes; project a's retry fails again.
    reporter.onTestCaseStart(cases, flaky);
    reporter.onTestCaseResult(cases, caseResult('flaky', 'passed', { invocations: 2 }));
    reporter.onTestFileResult(b, {
      testResults: [caseResult('flaky', 'passed', { invocations: 2 })], perfStats: {},
    }, {});
    reporter.onTestCaseStart(cases, flaky);
    reporter.onTestCaseResult(cases, caseResult('flaky', 'failed', { invocations: 2 }));
    reporter.onTestFileResult(a, {
      testResults: [caseResult('flaky', 'failed', { invocations: 2 }), caseResult('skipped', 'pending')],
      perfStats: {},
    }, {});
    reporter.onRunComplete(new Set(), {});
  });
  assertContract(records);
  const starts = records.filter((r) => r.event === 'group_started');
  assert.deepEqual(starts.map((r) => r.meta.project), ['a', 'b']);
  const groups = records.filter((r) => r.event === 'group_finished');
  assert.deepEqual(
    groups.map((r) => [r.group, r.meta.project, r.outcome, r.counts]),
    [
      ['__tests__/same.test.js', 'b', 'passed', { passed: 1 }],
      ['__tests__/same.test.js', 'a', 'failed', { failed: 1, skipped: 1 }],
    ],
  );
  // Each group start pairs with its own project's finish.
  for (const finish of groups) {
    const start = starts.find((r) => r.execution_id === finish.execution_id);
    assert.equal(start.meta.project, finish.meta.project);
  }
  // Every attempt is written, the undelivered skipped result once, and the
  // suite counts only final attempts, matching the groups.
  const tests = records.filter((r) => r.event === 'test_finished');
  assert.deepEqual(tests.map((r) => [r.name, r.outcome]), [
    ['flaky', 'failed'], ['flaky', 'failed'], ['flaky', 'passed'], ['flaky', 'failed'], ['skipped', 'skipped'],
  ]);
  const summary = records[records.length - 1];
  assert.deepEqual(summary.counts, { failed: 1, passed: 1, skipped: 1 });
  assert.equal(summary.unfinished_tests, 0);
  assert.equal(summary.unfinished_groups, 0);
});

test('lone surrogates become U+FFFD, so the report stays valid UTF-8', () => {
  for (const fallback of [false, true]) {
    const original = String.prototype.toWellFormed;
    if (fallback) delete String.prototype.toWellFormed;
    let records;
    let raw;
    try {
      records = synthetic((reporter, root) => {
        const file = testFile(root, 'surrogate.test.js');
        reporter.onTestFileStart(file);
        reporter.onTestCaseResult(file, caseResult('a\uD800b', 'passed', { ancestorTitles: ['\uDC00'] }));
        reporter.onTestFileResult(file, { testResults: [caseResult('a\uD800b', 'passed', { ancestorTitles: ['\uDC00'] })], perfStats: {} }, {});
        reporter.onRunComplete(new Set(), {});
      });
    } finally {
      if (fallback && original) String.prototype.toWellFormed = original;
    }
    assertContract(records);
    const record = finished(records, 'a�b');
    assert.ok(record, `fallback ${fallback}`);
    assert.deepEqual(record.path, ['�']);
    raw = JSON.stringify(records);
    assert.ok(!/\\ud[89a-f]/i.test(raw));
  }
});

test('text edge cases: NUL, empty titles and the 32-entry path cap', () => {
  const deep = Array.from({ length: 40 }, (_, i) => `level ${i}`);
  const results = [
    caseResult('nul\u0000name', 'passed', { ancestorTitles: ['describe\u0000title'] }),
    caseResult('', 'passed', { ancestorTitles: [''] }),
    caseResult('deep', 'passed', { ancestorTitles: deep }),
  ];
  const records = synthetic((reporter, root) => {
    const file = testFile(root, 'text.test.js');
    reporter.onTestFileStart(file);
    results.forEach((r) => reporter.onTestCaseResult(file, r));
    reporter.onTestFileResult(file, { testResults: results, perfStats: {} }, {});
    reporter.onRunComplete(new Set(), {});
  });
  assertContract(records);
  const nul = finished(records, 'nul�name');
  assert.deepEqual(nul.path, ['describe�title']);
  assert.ok(!JSON.stringify(records).includes('\\u0000'));
  const empty = finished(records, '-');
  assert.deepEqual(empty.path, ['-']);
  const capped = finished(records, 'deep');
  assert.equal(capped.path.length, 32);
  assert.deepEqual(capped.path, deep.slice(0, 32));
});

test('suite identity falls back to GOODFLOWS_RUN_ID, then the report id', () => {
  const runId = '0b9d3c1e-5a7f-4c2b-9e1d-2f3a4b5c6d7e';
  const [fromRun] = run({ env: { GOODFLOWS_RUN_ID: runId } }).records;
  assert.equal(fromRun.suite_id, runId);
  const [explicit] = run({ env: { GOODFLOWS_RUN_ID: runId, TEST_TIMING_SUITE_ID: 'nightly-42' } }).records;
  assert.equal(explicit.suite_id, 'nightly-42');
  const [invalid] = run({ env: { GOODFLOWS_RUN_ID: 'not/valid' } }).records;
  assert.equal(invalid.suite_id, invalid.report_id);
  // Report ids carry random bytes, so two processes with one PID still differ.
  assert.match(invalid.report_id, /^[0-9]+-[0-9]+-[0-9]+-[0-9a-f]{8}$/);
});

test('suite_started.meta names the reporter and whether tests were filtered', () => {
  const pkg = require('../package.json');
  const [started] = run().records;
  assert.equal(started.meta.reporter, 'goodflows-jest-reporter');
  assert.equal(started.meta.reporter_version, pkg.version);
  // The recorded run has no filter in its globalConfig.
  assert.equal(started.meta.filtered, false);
  for (const global of [
    { testNamePattern: '^(?!x)' },
    { testPathPatterns: { patterns: ['nested'] } },
    { testPathPattern: 'nested' },
    { onlyChanged: true },
    { onlyFailures: true },
  ]) {
    assert.equal(run({ global }).records[0].meta.filtered, true, JSON.stringify(global));
  }
  assert.equal(run({ global: { testPathPatterns: { patterns: [] }, testPathPattern: '' } }).records[0].meta.filtered, false);
});

test('an outputDirectory other than GOODFLOWS_REPORT_DIR wins, with one warning', () => {
  let result;
  const lines = captureStderr(() => {
    result = run({ options: { outputDirectory: 'custom' } });
  });
  const warnings = lines.filter((l) => l.includes('not GOODFLOWS_REPORT_DIR'));
  assert.equal(warnings.length, 1);
  assert.ok(warnings[0].includes(path.join(result.root, 'custom')));
  assert.ok(warnings[0].includes('so goodflows will not collect this report'));
  // The same directory, or no GOODFLOWS_REPORT_DIR, is silent.
  const quiet = captureStderr(() => {
    const root = tmp();
    withEnv({ GOODFLOWS_REPORT_DIR: path.join(root, 'same') }, () => {
      replay(new Reporter(recorded(root).globalConfig, { outputDirectory: path.join(root, 'same') }), root);
    });
  });
  assert.equal(quiet.length, 0);
});

test('short writes are retried until each line is complete', () => {
  const original = fs.writeSync;
  fs.writeSync = (fd, buffer, offset, length) => original(fd, buffer, offset, Math.min(length, 7));
  let records;
  try {
    records = run().records;
  } finally {
    fs.writeSync = original;
  }
  assertContract(records);
  assert.equal(records[records.length - 1].event, 'suite_finished');
});
