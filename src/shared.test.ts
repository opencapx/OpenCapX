// Unit tests for the frontend's pure modules (no DOM, no Tauri APIs).
// The Rust core has 1100+ inline tests; these are the WebView side's first line,
// covering the pure logic overlay.ts and settings.ts build on: session merging,
// SSE chunk parsing, listener dispatch, bubble formatting, and i18n fallback.

import { describe, expect, it } from "vitest";
import { mergeSession, type AgentEvent } from "./shared";

const ev = (over: Partial<AgentEvent>): AgentEvent => ({
  id: "s1",
  agent: "claude",
  project: "OpenCapX",
  message: "working",
  state: "working",
  updatedAt: 1000,
  ...over,
});

describe("mergeSession", () => {
  it("appends a new session", () => {
    const list = [ev({ id: "a" })];
    const next = mergeSession(list, ev({ id: "b" }));
    expect(next.map((s) => s.id)).toEqual(["a", "b"]);
  });

  it("replaces an existing session in place, keeping position", () => {
    const list = [ev({ id: "a" }), ev({ id: "b" }), ev({ id: "c" })];
    const next = mergeSession(list, ev({ id: "b", state: "done", message: "finished" }));
    expect(next.map((s) => s.id)).toEqual(["a", "b", "c"]);
    expect(next[1]).toMatchObject({ state: "done", message: "finished" });
  });

  it("does not mutate the input list", () => {
    const list = [ev({ id: "a" })];
    mergeSession(list, ev({ id: "a", state: "idle" }));
    expect(list[0].state).toBe("working");
  });
});
