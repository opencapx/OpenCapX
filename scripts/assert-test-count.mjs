// Asserts the Rust test suite's result line against a floor, so a silent test
// loss (fewer tests discovered — everything still green) fails CI instead of
// slipping through. The #20 incident: a truncated tests.rs dropped 31 tests and
// every check stayed green, because "fewer tests" is not "failing tests".
//
// Usage: node scripts/assert-test-count.mjs <log-file> <suite-name> <min-passed>
// The suite count is platform-dependent (mac-only/windows-only tests), so each
// CI job passes its own floor with headroom; a drop like #20's (31 tests) lands
// far below any of them.

import { readFileSync } from "node:fs";

const [logPath, suite, minRaw] = process.argv.slice(2);
const min = Number(minRaw);
if (!logPath || !suite || !Number.isFinite(min)) {
  console.error("usage: assert-test-count.mjs <log-file> <suite-name> <min-passed>");
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
  if (n < min) {
    console.error(`assert-test-count: ${n} passed is below the floor ${min} — did tests get lost?`);
    failed = true;
  }
}
process.exit(failed ? 1 : 0);
