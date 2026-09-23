#!/usr/bin/env node
// Conventional Commits gate (lefthook commit-msg). Spec: https://www.conventionalcommits.org
//   <type>[optional scope][!]: <description>
// Types follow the spec's common set; scope is lowercase alnum . _ - ; "!" marks a breaking change.
// Merge and auto-revert commits are exempt: git generates those subjects, not the author.
import { readFileSync } from "node:fs";

const TYPES = ["build", "chore", "ci", "docs", "feat", "fix", "perf", "refactor", "revert", "style", "test"];
const PATTERN = new RegExp(`^(${TYPES.join("|")})(\\([a-z0-9._-]+\\))?!?: (.+)`);

const path = process.argv[2];
if (!path) {
  console.error("check-commit-msg: no commit-msg file argument");
  process.exit(1);
}
const lines = readFileSync(path, "utf8").split("\n");
const subject = lines[0] ?? "";

const exempt = /^(Merge (branch|pull request|remote-tracking)|Revert ")/.test(subject);
const fail = (why) => {
  console.error(`✗ commit message rejected: ${why}`);
  console.error(`  subject: ${subject}`);
  console.error(`  expected shape: type[(scope)][!]: description   e.g. fix(bubble): apply the saved side at startup`);
  console.error(`  types: ${TYPES.join(" ")}`);
  process.exit(1);
};

if (exempt) process.exit(0);
if (!PATTERN.test(subject)) {
  fail(
    subject.includes(":")
      ? "unknown type or malformed scope (scope must be lowercase alnum . _ -)"
      : "missing '<type>: ' prefix",
  );
}
if (subject.length > 100) fail(`subject is ${subject.length} chars, keep it under 100`);
if (/:\s+$/.test(subject) || /^.{0,80}:\s{2,}/.test(subject)) fail("exactly one space after the colon");
