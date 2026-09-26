// Asserts the Rust test suite's result line against a floor, so a silent test
// loss (fewer tests discovered — everything still green) fails CI instead of
// slipping through. The #20 incident: a truncated tests.rs dropped 31 tests and
// every check stayed green, because "fewer tests" is not "failing tests".
//
// Usage: node scripts/assert-test-count.mjs <log-file> <suite-name>
// The floor lives here, not in ci.yml, so raising it is a reviewed change.

import { readFileSync } from "node:fs";

const MIN_RUST_BIN_TESTS = 1100;

const [logPath, suite] = process.argv.slice(2);
if (!logPath || !suite) {
  console.error("usage: assert-test-count.mjs <log-file> <suite-name>");
  process.exit(2);
}

const log = readFileSync(logPath, "utf8");
const lines = [...log.matchAll(/^test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored/gm)];
if (lines.length === 0) {
  console.error(`assert-test-count: no "test result:" line found in ${logPath}`);
  process.exit(1);
}

let failed = false;
for (const m of lines) {
  const [, status, passed, failedN, ignored] = m;
  const n = Number(passed);
  console.log(`assert-test-count [${suite}]: ${status}, ${passed} passed, ${failedN} failed, ${ignored} ignored`);
  if (status !== "ok" || Number(failedN) !== 0) failed = true;
  if (suite === "rust" && n < MIN_RUST_BIN_TESTS) {
    console.error(`assert-test-count: ${n} passed is below the floor ${MIN_RUST_BIN_TESTS} — did tests get lost?`);
    failed = true;
  }
}
process.exit(failed ? 1 : 0);
