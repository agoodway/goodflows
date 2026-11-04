'use strict';

const path = require('path');

// Jest bails once its failed-test count reaches `bail`; the reporter then
// writes max_failures_reached and an aborted suite_finished.
module.exports = {
  rootDir: __dirname,
  roots: ['<rootDir>/bail'],
  testEnvironment: 'node',
  bail: 1,
  reporters: ['default', [path.resolve(__dirname, '../../jest'), {}]],
};
