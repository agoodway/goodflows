'use strict';

const path = require('path');

// Two projects run the same files (as a project matrix over environments
// does), so one test path belongs to two groups, told apart by meta.project.
// retry.test.js retries a flaky test, so Jest reports each attempt.
const project = (name) => ({
  displayName: name,
  rootDir: path.join(__dirname, 'projects'),
  testEnvironment: 'node',
});

module.exports = {
  rootDir: __dirname,
  projects: [project('node-a'), project('node-b')],
  reporters: ['default', [path.resolve(__dirname, '../../jest'), {}]],
};
