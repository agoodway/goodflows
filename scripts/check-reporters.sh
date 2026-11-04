#!/usr/bin/env bash
# Drift guard for the bundled test reporters.
#
# Runs reporters/fixtures/goodflows.toml with the goodflows CLI built from this
# checkout and fails unless every job's `reports_collected` event collected
# exactly one report and rejected none, and that report is complete and says
# what the fixture should: its last record is `suite_finished` with the
# expected `aborted`, no unfinished tests or groups, and the expected suite and
# group counts. A reporter warning ("stopped writing") in a job's output also
# fails. The fixture suites fail on purpose, so the workflow's own exit status
# is ignored. No credentials are passed, so collection validates every report
# locally and nothing is uploaded.
#
# Prerequisites: Elixir (reporters/fixtures/exunit) and the Jest fixture's
# dependencies (`npm ci` in reporters/fixtures/jest).
# Set GOODFLOWS_BIN to use an already built CLI instead of `cargo build`.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [ -z "${GOODFLOWS_BIN:-}" ]; then
  cargo build --locked --quiet --manifest-path "$root/Cargo.toml"
  GOODFLOWS_BIN="$root/target/debug/goodflows"
fi

events="$(mktemp)"
trap 'rm -f "$events"' EXIT

env -u GOODFLOWS_FIXTURE_URL -u GOODFLOWS_FIXTURE_API_KEY \
  -u GOODFLOWS_REPORT_DIR -u GOODFLOWS_RUN_ID \
  "$GOODFLOWS_BIN" --config "$root/reporters/fixtures/goodflows.toml" --json --no-history \
  >"$events" || true

node - "$events" <<'NODE'
'use strict';
const fs = require('fs');
const path = require('path');

// What each fixture job's report must say. `groups` lists every group as
// [group, outcome, counts], in any order.
const expected = {
  exunit: {
    aborted: false,
    counts: { passed: 5, failed: 2, skipped: 4, errored: 1 },
    groups: [
      ['Fixture.OutcomesTest', 'failed', { passed: 3, failed: 2, skipped: 2 }],
      ['Fixture.ParameterizedTest', 'passed', { passed: 1 }],
      ['Fixture.ParameterizedTest', 'passed', { passed: 1 }],
      ['Fixture.SetupAllTest', 'errored', { errored: 1 }],
      ['Fixture.SkippedTest', 'skipped', { skipped: 2 }],
    ],
  },
  jest: {
    aborted: false,
    counts: { passed: 4, failed: 1, skipped: 3 },
    groups: [
      ['__tests__/outcomes.test.js', 'failed', { passed: 1, failed: 1, skipped: 3 }],
      ['__tests__/load-error.test.js', 'errored', {}],
      ['__tests__/nested.test.js', 'passed', { passed: 3 }],
    ],
  },
  // Each project runs both files; the flaky test's failed first attempt is
  // written but not counted.
  'jest-projects': {
    aborted: false,
    counts: { passed: 6 },
    tests: 8,
    projects: ['node-a', 'node-b'],
    groups: [
      ['projects/__tests__/same.test.js', 'passed', { passed: 1 }],
      ['projects/__tests__/same.test.js', 'passed', { passed: 1 }],
      ['projects/__tests__/retry.test.js', 'passed', { passed: 2 }],
      ['projects/__tests__/retry.test.js', 'passed', { passed: 2 }],
    ],
  },
  'jest-bail': {
    aborted: true,
    maxFailures: true,
    counts: { failed: 2 },
    groups: [['bail/__tests__/fails.test.js', 'failed', { failed: 2 }]],
  },
};

// Order-insensitive JSON, so counts compare by content.
const canonical = (value) =>
  JSON.stringify(value, (_key, v) =>
    v && typeof v === 'object' && !Array.isArray(v)
      ? Object.fromEntries(Object.entries(v).sort(([a], [b]) => (a < b ? -1 : 1)))
      : v,
  );
const sorted = (rows) => rows.map(canonical).sort();

function findReport(dir, name) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      const found = findReport(full, name);
      if (found) return found;
    } else if (entry.name === name) {
      return full;
    }
  }
  return null;
}

function checkReport(want, file, problems) {
  const records = fs
    .readFileSync(file, 'utf8')
    .split('\n')
    .filter((line) => line.trim() !== '')
    .map((line) => JSON.parse(line));
  const last = records[records.length - 1];
  if (!last || last.event !== 'suite_finished') {
    problems.push(`report is truncated: last record is ${last ? last.event : 'missing'}, not suite_finished`);
    return;
  }
  if (last.aborted !== want.aborted) problems.push(`suite_finished.aborted is ${last.aborted}, expected ${want.aborted}`);
  if (last.unfinished_tests !== 0 || last.unfinished_groups !== 0) {
    problems.push(`unfinished tests/groups: ${last.unfinished_tests}/${last.unfinished_groups}, expected 0/0`);
  }
  if (canonical(last.counts) !== canonical(want.counts)) {
    problems.push(`suite counts ${canonical(last.counts)}, expected ${canonical(want.counts)}`);
  }
  const maxFailures = records.filter((r) => r.event === 'max_failures_reached').length;
  if (maxFailures !== (want.maxFailures ? 1 : 0)) {
    problems.push(`${maxFailures} max_failures_reached record(s), expected ${want.maxFailures ? 1 : 0}`);
  }
  const groups = records.filter((r) => r.event === 'group_finished');
  const got = sorted(groups.map((r) => [r.group, r.outcome, r.counts]));
  const exp = sorted(want.groups);
  if (canonical(got) !== canonical(exp)) {
    problems.push(`groups ${got.join(' ')}, expected ${exp.join(' ')}`);
  }
  if (want.tests !== undefined) {
    const tests = records.filter((r) => r.event === 'test_finished').length;
    if (tests !== want.tests) problems.push(`${tests} test_finished records, expected ${want.tests}`);
  }
  if (want.projects) {
    const projects = [...new Set(groups.map((r) => r.meta && r.meta.project))].sort();
    if (canonical(projects) !== canonical(want.projects)) {
      problems.push(`projects ${canonical(projects)}, expected ${canonical(want.projects)}`);
    }
  }
}

const events = fs
  .readFileSync(process.argv[2], 'utf8')
  .split('\n')
  .filter((line) => line.trim() !== '')
  .map((line) => JSON.parse(line));
const collected = new Map(
  events.filter((e) => e.type === 'reports_collected').map((e) => [e.job, e]),
);
let failures = 0;
for (const [job, want] of Object.entries(expected)) {
  const event = collected.get(job);
  const problems = [];
  const warnings = events
    .filter((e) => e.type === 'job_output' && e.name === job && /stopped writing/.test(e.data || ''))
    .map((e) => e.data.trim());
  for (const warning of warnings) problems.push(`reporter warning: ${warning}`);
  if (!event) {
    problems.push('no reports_collected event (did the job run?)');
  } else {
    const rejected = Object.entries(event.rejected || {});
    if (rejected.length > 0) {
      problems.push(`rejected reports: ${rejected.map(([code, n]) => `${code} x${n}`).join(', ')}`);
    }
    if (event.reports !== 1) {
      problems.push(`${event.reports} reports collected, expected 1 (outcome ${event.outcome})`);
    }
    for (const id of event.report_ids || []) {
      const file = findReport(event.run_dir, `${id}.jsonl`);
      if (file) checkReport(want, file, problems);
      else problems.push(`report ${id} not found under ${event.run_dir}`);
    }
  }
  for (const problem of problems) {
    console.error(`${job}: ${problem}`);
  }
  if (problems.length === 0) {
    console.log(`${job}: 1 report collected, none rejected, complete with the expected counts`);
  }
  failures += problems.length;
}
process.exit(failures > 0 ? 1 : 0);
NODE
