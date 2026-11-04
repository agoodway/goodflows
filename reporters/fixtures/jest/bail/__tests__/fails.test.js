'use strict';

// Two failures in one file reach `bail: 1`, so Jest stops after this file.
test('fails first', () => {
  expect(1).toBe(2);
});

test('fails second', () => {
  expect(2).toBe(3);
});
