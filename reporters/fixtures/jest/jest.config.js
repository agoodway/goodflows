'use strict';

const path = require('path');

module.exports = {
  testEnvironment: 'node',
  displayName: 'fixture',
  // projects/ and bail/ belong to the other configs (jest.projects.config.js,
  // jest.bail.config.js), which goodflows.toml runs as their own jobs.
  roots: ['<rootDir>/__tests__'],
  // Jest has no tags; a name filter stands in for one, so one test is skipped by it.
  testNamePattern: '^(?!.*filtered out by name)',
  reporters: ['default', [path.resolve(__dirname, '../../jest'), {}]],
};
