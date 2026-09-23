// Unit tests for events.ts: SSE chunk parsing and local listener dispatch.
// parseSseChunk is the wire-format contract with core's tiny_http SSE stream;
// onEvent/emitLocal is the dispatch every settings tab subscribes through.

import { describe, expect, it, vi } from "vitest";
import { emitLocal, onEvent, parseSseChunk, type OpencapxEvent } from "./events";

const mk = (over: Partial<OpencapxEvent>): OpencapxEvent => ({
  id: "e1",
  kind: "permission.ask",
  source: "core",
  timestamp: 1234,
  payload: null,
  ...over,
});

describe("parseSseChunk", () => {
  it("parses event/data/id fields with the SSE single-space convention", () => {
    const e = parseSseChunk('event: permission.ask\nid: 42\ndata: {"kind":"permission.ask","id":"42"}\n');
    expect(e).not.toBeNull();
    expect(e!.id).toBe("42");
    expect(e!.kind).toBe("permission.ask");
  });

  it("skips comment/heartbeat lines and field-less lines", () => {
    const e = parseSseChunk(': heartbeat\n\nx\n\nevent: pet.state\ndata: {"payload":{"a":1}}\n');
    expect(e).not.toBeNull();
    expect(e!.kind).toBe("pet.state");
    expect(e!.payload).toEqual({ a: 1 });
  });

  it("joins multi-line data with newlines before JSON.parse", () => {
    const e = parseSseChunk('event: pet.state\ndata: {"payload":\ndata: {"a":1}}\n');
    expect(e).not.toBeNull();
    expect(e!.payload).toEqual({ a: 1 });
  });

  it("rejects chunks without both event and data", () => {
    expect(parseSseChunk("data: {}")).toBeNull();
    expect(parseSseChunk("event: pet.state")).toBeNull();
  });

  it("rejects non-JSON and non-object data", () => {
    expect(parseSseChunk("event: pet.state\ndata: not json")).toBeNull();
    expect(parseSseChunk("event: pet.state\ndata: 42")).toBeNull();
  });

  it("prefers the id/kind embedded in the JSON body over the SSE fields", () => {
    const e = parseSseChunk('event: message\nid: 1\ndata: {"id":"inner","kind":"plugin.metrics.sampled"}');
    expect(e!.id).toBe("inner");
    expect(e!.kind).toBe("plugin.metrics.sampled");
    expect(e!.source).toBe("");
    expect(e!.timestamp).toBe(0);
  });
});

describe("onEvent / emitLocal", () => {
  it("dispatches to listeners registered for the kind", () => {
    const cb = vi.fn();
    onEvent("pet.state", cb);
    emitLocal(mk({ kind: "pet.state" }));
    emitLocal(mk({ kind: "permission.ask" }));
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it("delivers every event to '*' listeners", () => {
    const all = vi.fn();
    onEvent("*", all);
    emitLocal(mk({ kind: "pet.state" }));
    emitLocal(mk({ kind: "permission.ask" }));
    expect(all).toHaveBeenCalledTimes(2);
  });

  it("isolates a throwing listener from the others", () => {
    const err = vi.spyOn(console, "error").mockImplementation(() => {});
    const good = vi.fn();
    onEvent("pet.bubble", () => {
      throw new Error("boom");
    });
    onEvent("pet.bubble", good);
    emitLocal(mk({ kind: "pet.bubble" }));
    expect(good).toHaveBeenCalledTimes(1);
    expect(err).toHaveBeenCalled();
    err.mockRestore();
  });

  it("stops delivering after the unsubscribe fn is called", () => {
    const cb = vi.fn();
    const off = onEvent("pet.state", cb);
    off();
    emitLocal(mk({ kind: "pet.state" }));
    expect(cb).not.toHaveBeenCalled();
  });
});
