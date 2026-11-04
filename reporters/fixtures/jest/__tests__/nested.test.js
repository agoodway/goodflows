'use strict';

describe('math', () => {
  describe('sum', () => {
    test('adds', () => {
      expect(1 + 2).toBe(3);
    });

    test('retries', () => {
      expect([1, 2].length).toBe(2);
    });
  });

  test('multiplies', () => {
    expect(2 * 3).toBe(6);
  });
});
