/** A Jest reporter that writes each run as a goodflows `test-timings/1` JSON Lines report. */
declare class GoodflowsJestReporter {
  constructor(
    globalConfig: unknown,
    options?: GoodflowsJestReporter.Options,
    context?: unknown,
  );
  onRunStart(aggregatedResults: unknown, options?: unknown): void;
  onTestFileStart(test: unknown): void;
  onTestCaseStart(test: unknown, testCaseStartInfo: unknown): void;
  onTestCaseResult(test: unknown, testCaseResult: unknown): void;
  onTestFileResult(test: unknown, testResult: unknown, aggregatedResults: unknown): void;
  onRunComplete(testContexts: unknown, aggregatedResults: unknown): void;
  getLastError(): Error | undefined;
}

declare namespace GoodflowsJestReporter {
  /** Options given as the reporter's second tuple element in Jest's `reporters`. */
  interface Options {
    /**
     * Directory for `<report_id>.jsonl` reports, resolved against Jest's `rootDir`.
     * Defaults to a nonempty `GOODFLOWS_REPORT_DIR`, then `<rootDir>/.goodflows/test-timings`.
     * When set, it wins over `GOODFLOWS_REPORT_DIR`; if the two differ, the reporter
     * warns that goodflows will not collect the report.
     */
    outputDirectory?: string;
    /**
     * Suite identity shared by related reports (for example every shard of one run).
     * Must follow the identifier grammar: 1-128 bytes of `[A-Za-z0-9._-]`, starting
     * with a letter or digit. Defaults to a valid `TEST_TIMING_SUITE_ID`, then a valid
     * `GOODFLOWS_RUN_ID` (set by goodflows), then the report id.
     */
    suiteId?: string;
  }
}

export = GoodflowsJestReporter;
