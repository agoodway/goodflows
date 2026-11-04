'use strict';

// goodflows Jest reporter: writes each run as a `test-timings/1` JSON Lines
// report that goodflows collects. Zero runtime dependencies, CommonJS, and no
// build step. Every hook catches its own errors: a reporter failure prints one
// warning and stops writing for that run, and never changes test results.

const crypto = require('crypto');
const fs = require('fs');
const path = require('path');

const { name: REPORTER_NAME, version: REPORTER_VERSION } = require('./package.json');

const FORMAT = 'test-timings/1';
const MAX_SAFE_INTEGER = Number.MAX_SAFE_INTEGER;
const MAX_TEXT_BYTES = 4096;
const MAX_META_STRING_BYTES = 256;
const MAX_RUNNER_VERSION_BYTES = 128;
const MAX_PATH_ENTRIES = 32;
const IDENTIFIER = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
const PARTITION = /^[A-Za-z0-9._-]{1,64}$/;
const SKIPPED = new Set(['pending', 'todo', 'disabled', 'focused', 'skipped']);

let sequence = 0;

function identifier(value) {
  return typeof value === 'string' && IDENTIFIER.test(value) ? value : null;
}

function present(name) {
  const value = process.env[name];
  return typeof value === 'string' && value !== '' ? value : null;
}

function positiveInteger(value) {
  if (typeof value === 'string' && /^[0-9]+$/.test(value)) {
    const n = Number(value);
    return Number.isSafeInteger(n) && n >= 1 ? n : null;
  }
  return Number.isSafeInteger(value) && value >= 1 ? value : null;
}

function safeInteger(value) {
  return Number.isSafeInteger(value) ? value : null;
}

// A lone UTF-16 surrogate has no UTF-8 encoding, and JSON.stringify would
// write it as a bare `\uD800` escape that collection rejects, so it becomes
// U+FFFD. Node 18 lacks String.prototype.toWellFormed; Buffer's UTF-8 encoder
// makes the same replacement.
function wellFormed(value) {
  return typeof value.toWellFormed === 'function'
    ? value.toWellFormed()
    : Buffer.from(value, 'utf8').toString('utf8');
}

// Cut a string to at most `limit` UTF-8 bytes on a character boundary.
function truncate(value, limit) {
  const text = wellFormed(String(value)).replace(/\u0000/g, '�');
  const bytes = Buffer.from(text, 'utf8');
  if (bytes.length <= limit) {
    return text;
  }
  let end = limit;
  while (end > 0 && (bytes[end] & 0xc0) === 0x80) {
    end -= 1;
  }
  return bytes.subarray(0, end).toString('utf8');
}

// Core text is 1 to 4,096 bytes.
function text(value) {
  const cut = truncate(value == null ? '' : value, MAX_TEXT_BYTES);
  return cut === '' ? '-' : cut;
}

// Nonnegative integer microseconds from Jest's millisecond values.
function micros(ms) {
  if (typeof ms !== 'number' || !Number.isFinite(ms) || ms < 0) {
    return null;
  }
  const us = Math.round(ms * 1000);
  return us <= MAX_SAFE_INTEGER ? us : null;
}

function between(start, end) {
  return typeof start === 'number' && typeof end === 'number' && end >= start
    ? micros(end - start)
    : null;
}

// A path inside `root`, relative to it, or null.
function relative(root, file) {
  if (typeof file !== 'string' || file === '') {
    return null;
  }
  const rel = path.relative(root, path.resolve(root, file)).split(path.sep).join('/');
  if (rel === '' || rel.startsWith('..') || path.isAbsolute(rel)) {
    return null;
  }
  return Buffer.byteLength(rel, 'utf8') <= MAX_TEXT_BYTES ? rel : null;
}

function outcome(status) {
  if (status === 'passed' || status === 'failed') {
    return status;
  }
  return SKIPPED.has(status) ? 'skipped' : 'errored';
}

function increment(counts, key) {
  counts[key] = (counts[key] || 0) + 1;
}

function runnerVersion(rootDir) {
  try {
    const manifest = require.resolve('jest/package.json', { paths: [rootDir] });
    const { version } = JSON.parse(fs.readFileSync(manifest, 'utf8'));
    return typeof version === 'string' ? truncate(version, MAX_RUNNER_VERSION_BYTES) : null;
  } catch (_error) {
    return null;
  }
}

function warn(message) {
  try {
    process.stderr.write(`warning: goodflows-jest-reporter ${message}\n`);
  } catch (_error) {
    // Never throw out of a hook.
  }
}

// Whether Jest was asked to run only some tests (by name, path, changed files
// or earlier failures). Sharding is a partition, not a filter.
function filtered(globalConfig) {
  const patterns = globalConfig.testPathPatterns;
  const pathPatterns = Array.isArray(patterns)
    ? patterns
    : (patterns && Array.isArray(patterns.patterns) && patterns.patterns) || [];
  return Boolean(
    globalConfig.testNamePattern ||
      globalConfig.testPathPattern ||
      pathPatterns.length ||
      globalConfig.onlyChanged ||
      globalConfig.onlyFailures ||
      globalConfig.findRelatedTests ||
      globalConfig.lastCommit ||
      globalConfig.changedSince ||
      globalConfig.filter,
  );
}

// A project's identity, so the same file run by two projects stays two groups.
// Only file events carry the right project: Jest gives case events the context
// of whichever project created the test runner, so cases are keyed by path.
function projectKey(test) {
  const config = (test && test.context && test.context.config) || {};
  if (typeof config.id === 'string' && config.id !== '') {
    return config.id;
  }
  const name = config.displayName && config.displayName.name;
  return JSON.stringify([config.rootDir || null, typeof name === 'string' ? name : null]);
}

function groupKey(test) {
  return JSON.stringify([projectKey(test), (test && test.path) || null]);
}

class GoodflowsJestReporter {
  constructor(globalConfig, options) {
    this._globalConfig = globalConfig || {};
    this._options = options || {};
    this._run = null;
  }

  // ---- Jest hooks ----

  onRunStart(aggregatedResults) {
    // A new run starts writing again, even after an earlier run failed.
    this._guard(() => this._start(aggregatedResults || {}), true);
  }

  onTestFileStart(test) {
    this._guard(() => this._fileStart(test));
  }

  onTestCaseStart(test, info) {
    this._guard(() => this._caseStart(test, info || {}));
  }

  onTestCaseResult(test, result) {
    this._guard(() => this._caseResult(test, result || {}));
  }

  onTestFileResult(test, testResult, aggregatedResults) {
    this._guard(() => this._fileResult(test, testResult || {}, aggregatedResults || {}));
  }

  onRunComplete(_contexts, aggregatedResults) {
    this._guard(() => this._complete(aggregatedResults || {}));
  }

  getLastError() {
    return undefined;
  }

  // ---- run state ----

  _guard(fn, starting = false) {
    if (!starting && this._run && this._run.disabled) {
      return;
    }
    try {
      fn();
    } catch (error) {
      this._disable(error);
    }
  }

  _disable(error) {
    const run = this._run;
    if (!run || run.disabled) {
      return;
    }
    run.disabled = true;
    if (run.fd != null) {
      try {
        fs.closeSync(run.fd);
      } catch (_error) {
        // Already reported once; nothing else to do.
      }
      run.fd = null;
    }
    const message = error && error.message ? error.message : String(error);
    warn(`stopped writing: ${message}`);
  }

  // The explicit option wins over GOODFLOWS_REPORT_DIR. When both are set and
  // differ, goodflows cannot collect the report, so that is warned about (once
  // per reporter) and the report is still written where the option says.
  _directory() {
    const rootDir = this._root();
    const explicit = this._options.outputDirectory;
    const goodflows = present('GOODFLOWS_REPORT_DIR');
    if (typeof explicit === 'string' && explicit !== '') {
      const directory = path.resolve(rootDir, explicit);
      if (goodflows && path.resolve(goodflows) !== directory && !this._warnedDirectory) {
        this._warnedDirectory = true;
        warn(
          `writes to ${directory} (outputDirectory), not GOODFLOWS_REPORT_DIR (${goodflows}), ` +
            'so goodflows will not collect this report',
        );
      }
      return directory;
    }
    return goodflows || path.join(rootDir, '.goodflows', 'test-timings');
  }

  _partition() {
    // goodflows sets both variables together; unless both are valid, neither is
    // used and Jest's own sharding applies.
    const partition = present('GOODFLOWS_PARTITION');
    const count = positiveInteger(present('GOODFLOWS_PARTITION_COUNT'));
    if (partition && PARTITION.test(partition) && count) {
      return [partition, count];
    }
    const shard = this._globalConfig.shard;
    if (shard && positiveInteger(shard.shardIndex) && positiveInteger(shard.shardCount)) {
      return [String(shard.shardIndex), shard.shardCount];
    }
    return [null, 1];
  }

  _start(aggregated) {
    // Each run (including each watch-mode rerun) is a new report.
    if (this._run && this._run.fd != null && !this._run.disabled) {
      fs.closeSync(this._run.fd);
    }
    // Random bytes keep report ids unique across containers that share a PID.
    const reportId = `${Date.now()}-${process.pid}-${++sequence}-${crypto.randomBytes(4).toString('hex')}`;
    const suiteId =
      identifier(this._options.suiteId) ||
      identifier(present('TEST_TIMING_SUITE_ID')) ||
      identifier(present('GOODFLOWS_RUN_ID')) ||
      reportId;
    const startMs = typeof aggregated.startTime === 'number' && aggregated.startTime > 0
      ? aggregated.startTime
      : Date.now();
    this._run = {
      disabled: false,
      fd: null,
      reportId,
      suiteId,
      startMs,
      nextId: 1,
      // Open group executions per project and file, oldest first.
      groups: new Map(),
      tests: new Map(),
      // Per test path: case results delivered (so a file's result can write the
      // rest) and each case's counted attempt outcomes (so a retry replaces one).
      cases: new Map(),
      // Final-attempt counts: a retried test's earlier attempt is uncounted.
      counts: {},
      maxFailures: false,
      interrupted: false,
    };
    const directory = this._directory();
    fs.mkdirSync(directory, { recursive: true });
    this._run.fd = fs.openSync(path.join(directory, `${reportId}.jsonl`), 'wx');
    const rootDir = this._root();
    const [partition, partitionCount] = this._partition();
    const meta = {
      reporter: REPORTER_NAME,
      reporter_version: truncate(REPORTER_VERSION, MAX_META_STRING_BYTES),
      node_version: truncate(process.version, MAX_META_STRING_BYTES),
      max_workers: safeInteger(this._globalConfig.maxWorkers),
      duration_resolution_us: 1000,
      filtered: filtered(this._globalConfig),
    };
    this._write({
      event: 'suite_started',
      format: FORMAT,
      started_at: new Date(startMs).toISOString(),
      runner: { name: 'jest', version: runnerVersion(rootDir) },
      partition,
      partition_count: partitionCount,
      seed: safeInteger(this._globalConfig.seed),
      filters: null,
      meta,
    });
  }

  _now() {
    return micros(Date.now() - this._run.startMs) || 0;
  }

  _offset(ms) {
    return typeof ms === 'number' ? micros(ms - this._run.startMs) || 0 : this._now();
  }

  _write(record) {
    const run = this._run;
    if (!run || run.disabled || run.fd == null) {
      return;
    }
    const line = JSON.stringify({
      ...record,
      suite_id: run.suiteId,
      report_id: run.reportId,
      received_offset_us: this._now(),
    });
    // writeSync may write less than asked, so write until the line is complete.
    const buffer = Buffer.from(`${line}\n`, 'utf8');
    let written = 0;
    while (written < buffer.length) {
      written += fs.writeSync(run.fd, buffer, written, buffer.length - written);
    }
  }

  // Groups are named relative to the global Jest root directory, even in a
  // multi-project run, so same-named files in two projects stay distinct groups.
  _root() {
    return this._globalConfig.rootDir || process.cwd();
  }

  _groupFields(test) {
    const file = relative(this._root(), test && test.path);
    return {
      group: text(file || path.basename(String((test && test.path) || '')) || '-'),
      file,
    };
  }

  _groupMeta(test) {
    const config = (test && test.context && test.context.config) || {};
    const name = config.displayName && config.displayName.name;
    return typeof name === 'string' && name !== ''
      ? { project: truncate(name, MAX_META_STRING_BYTES) }
      : undefined;
  }

  _fileStart(test) {
    const run = this._run;
    if (!run) {
      return;
    }
    const started = {
      ...this._groupFields(test),
      execution_id: run.nextId++,
      started_offset_us: this._now(),
    };
    const key = groupKey(test);
    const open = run.groups.get(key) || [];
    open.push(started);
    run.groups.set(key, open);
    this._write({ event: 'group_started', ...started, meta: this._groupMeta(test) });
  }

  _cases(test) {
    const file = test && test.path;
    let cases = this._run.cases.get(file);
    if (!cases) {
      cases = { reported: new Map(), attempts: new Map() };
      this._run.cases.set(file, cases);
    }
    return cases;
  }

  _caseKey(test, info) {
    const titles = Array.isArray(info.ancestorTitles) ? info.ancestorTitles : [];
    return JSON.stringify([test && test.path, titles, info.title]);
  }

  _caseStart(test, info) {
    const run = this._run;
    if (!run) {
      return;
    }
    const titles = Array.isArray(info.ancestorTitles) ? info.ancestorTitles : [];
    const { group, file } = this._groupFields(test);
    const started = {
      group,
      path: titles.slice(0, MAX_PATH_ENTRIES).map(text),
      name: text(info.title),
      file,
      line: null,
      execution_id: run.nextId++,
      started_offset_us: this._offset(info.startedAt),
    };
    const key = this._caseKey(test, info);
    const open = run.tests.get(key) || [];
    open.push(started);
    run.tests.set(key, open);
    this._write({ event: 'test_started', ...started });
  }

  _caseResult(test, result, delivered = true) {
    const run = this._run;
    if (!run) {
      return;
    }
    const key = this._caseKey(test, result);
    const cases = this._cases(test);
    const attempts = cases.attempts.get(key) || [];
    cases.attempts.set(key, attempts);
    const result_outcome = outcome(result.status);
    // jest.retryTimes delivers a start and a result per attempt, and a later
    // attempt has more invocations. Every attempt is written, but only the final
    // one is counted, as in the file's own results: a retry uncounts the
    // earlier failed attempt it replaces.
    let replaced = -1;
    if (delivered && Number.isSafeInteger(result.invocations) && result.invocations > 1) {
      for (let i = attempts.length - 1; i >= 0 && replaced < 0; i -= 1) {
        if (attempts[i] === 'failed' || attempts[i] === 'errored') replaced = i;
      }
    }
    if (replaced >= 0) {
      const [previous] = attempts.splice(replaced, 1);
      run.counts[previous] -= 1;
      if (run.counts[previous] === 0) {
        delete run.counts[previous];
      }
    } else if (delivered) {
      cases.reported.set(key, (cases.reported.get(key) || 0) + 1);
    }
    attempts.push(result_outcome);
    increment(run.counts, result_outcome);
    // Jest does not start skipped or todo tests, so a start is written first.
    if (!(run.tests.get(key) || []).length) {
      this._caseStart(test, { ...result, startedAt: undefined });
    }
    const open = run.tests.get(key);
    const started = open.shift();
    if (!open.length) {
      run.tests.delete(key);
    }
    const meta = {};
    if (result_outcome === 'skipped') {
      meta.skip_reason = truncate(result.status, MAX_META_STRING_BYTES);
    }
    if (Number.isSafeInteger(result.invocations) && result.invocations > 1) {
      meta.invocations = result.invocations;
    }
    const line = result.location && result.location.line;
    if (Number.isSafeInteger(line) && line >= 1) {
      meta.line = line;
    }
    this._write({
      event: 'test_finished',
      ...started,
      duration_us: micros(result.duration),
      outcome: result_outcome,
      meta: Object.keys(meta).length ? meta : undefined,
    });
  }

  _fileResult(test, testResult, aggregated) {
    const run = this._run;
    if (!run) {
      return;
    }
    const key = groupKey(test);
    // Jest delivers no case events for skipped tests (and for some others), so
    // every result the file reports that was not delivered is written now.
    // Delivered results are consumed one per final result, so the same file
    // finishing in another project still finds its own.
    const cases = this._cases(test);
    for (const result of testResult.testResults || []) {
      const caseKey = this._caseKey(test, result);
      const seen = cases.reported.get(caseKey) || 0;
      if (seen > 0) {
        cases.reported.set(caseKey, seen - 1);
      } else {
        this._caseResult(test, result, false);
      }
      // This result's attempt is final now, so a later retry cannot replace it.
      const attempts = cases.attempts.get(caseKey) || [];
      const final = attempts.lastIndexOf(outcome(result && result.status));
      attempts.splice(final >= 0 ? final : 0, 1);
    }
    for (const [caseKey, seen] of cases.reported) {
      if (seen === 0) cases.reported.delete(caseKey);
    }
    for (const [caseKey, attempts] of cases.attempts) {
      if (!attempts.length) cases.attempts.delete(caseKey);
    }
    if (!cases.reported.size && !cases.attempts.size) {
      run.cases.delete(test && test.path);
    }
    const open = run.groups.get(key) || [];
    const started = open.shift();
    if (!open.length) {
      run.groups.delete(key);
    }
    if (started) {
      const counts = {};
      const outcomes = (testResult.testResults || []).map((r) => outcome(r.status));
      outcomes.forEach((o) => increment(counts, o));
      let group = 'passed';
      if (testResult.testExecError) {
        group = 'errored';
      } else if (outcomes.includes('failed') || outcomes.includes('errored')) {
        group = 'failed';
      } else if (outcomes.length && outcomes.every((o) => o === 'skipped')) {
        group = 'skipped';
      }
      const perf = testResult.perfStats || {};
      const meta = { ...this._groupMeta(test) };
      const environment = between(perf.loadTestEnvironmentStart, perf.loadTestEnvironmentEnd);
      const setup = between(perf.setupFilesStart, perf.setupFilesEnd);
      if (environment != null) meta.load_test_environment_us = environment;
      if (setup != null) meta.setup_files_us = setup;
      if (typeof perf.slow === 'boolean') meta.slow = perf.slow;
      this._write({
        event: 'group_finished',
        ...started,
        duration_us:
          typeof perf.runtime === 'number' ? micros(perf.runtime) : between(perf.start, perf.end),
        outcome: group,
        counts,
        meta,
      });
    }
    // Jest bails when its failed-test count (not its failed-file count)
    // reaches `bail`, so a file that fails to load does not count.
    const bail = this._globalConfig.bail;
    if (
      !run.maxFailures &&
      Number.isSafeInteger(bail) &&
      bail > 0 &&
      (aggregated.numFailedTests || 0) >= bail
    ) {
      run.maxFailures = true;
      this._write({ event: 'max_failures_reached' });
    }
  }

  _complete(aggregated) {
    const run = this._run;
    if (!run) {
      return;
    }
    if (aggregated.wasInterrupted) {
      run.interrupted = true;
      this._write({ event: 'interrupted' });
    }
    const unfinished = (open) => [...open.values()].reduce((n, list) => n + list.length, 0);
    this._write({
      event: 'suite_finished',
      aborted: run.interrupted || run.maxFailures,
      unfinished_tests: unfinished(run.tests),
      unfinished_groups: unfinished(run.groups),
      counts: run.counts,
      duration_us: micros(Date.now() - run.startMs),
    });
    if (run.fd != null) {
      fs.closeSync(run.fd);
      run.fd = null;
    }
  }
}

module.exports = GoodflowsJestReporter;
module.exports.default = GoodflowsJestReporter;
