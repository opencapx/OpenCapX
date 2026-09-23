// Conventional Commits, enforced by commitlint on the commit-msg lefthook.
// config-conventional supplies the spec shape (type-enum, subject rules, footer parsing,
// BREAKING CHANGE detection). Only repo-specific adjustments live here.
export default {
  extends: ["@commitlint/config-conventional"],
  rules: {
    // config-conventional has no scope-case rule, so `feat(API): …` would pass; the repo
    // convention (CONTRIBUTING.md "Commit style") is a lowercase scope.
    "scope-case": [2, "always", "lower-case"],
    // Same class of gap: config-conventional does not pair `!` with the footer, but the repo
    // requires both — `!` in the header and a `BREAKING CHANGE:` footer paragraph.
    "breaking-change-exclamation-mark": [2, "always"],
  },
};
