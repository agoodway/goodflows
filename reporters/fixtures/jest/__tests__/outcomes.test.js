'use strict';

test('passes', () => {
  expect(1 + 1).toBe(2);
});

test('fails on purpose', () => {
  expect('private assertion value').toBe('another value');
});

test.skip('is skipped', () => {
  expect(true).toBe(false);
});

test.todo('is a todo');

test('is filtered out by name', () => {
  expect(true).toBe(true);
});
