'use strict';

// The first attempt fails and the retry passes, so Jest reports two attempts
// of one test, and only the passing one counts.
jest.retryTimes(1);

let attempts = 0;

test('is flaky', () => {
  attempts += 1;
  expect(attempts).toBeGreaterThan(1);
});

test('is steady', () => {
  expect(true).toBe(true);
});
