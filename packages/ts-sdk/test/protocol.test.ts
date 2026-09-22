import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { PassThrough } from "node:stream";
import {
  ERR_INTERNAL,
  ERR_INVALID_PARAMS,
  ERR_INVALID_REQUEST,
  ERR_METHOD_NOT_FOUND,
  ERR_PARSE,
  ProtocolError,
  buildError,
  buildNotification,
  buildRequest,
  buildResult,
  decodeLine,
  writeMessage,
} from "../src/protocol.js";

describe("decodeLine", () => {
  it("parses a normal line", () => {
    assert.deepEqual(decodeLine('{"jsonrpc":"2.0","id":1}'), {
      jsonrpc: "2.0",
      id: 1,
    });
  });

  it("skips blank and whitespace-only lines", () => {
    assert.equal(decodeLine(""), null);
    assert.equal(decodeLine("   \n"), null);
  });

  it("throws ProtocolError on malformed json", () => {
    assert.throws(() => decodeLine("{ not json"), ProtocolError);
  });

  it("throws ProtocolError on non-object payloads", () => {
    assert.throws(() => decodeLine("[1,2]"), ProtocolError);
    assert.throws(() => decodeLine("42"), ProtocolError);
  });
});

describe("builders", () => {
  it("buildResult", () => {
    assert.deepEqual(buildResult(1, { ok: true }), {
      jsonrpc: "2.0",
      id: 1,
      result: { ok: true },
    });
  });

  it("buildError omits data when absent", () => {
    assert.deepEqual(buildError(1, -32601, "nope"), {
      jsonrpc: "2.0",
      id: 1,
      error: { code: -32601, message: "nope" },
    });
    const withData = buildError(1, -32603, "boom", { x: 1 });
    assert.deepEqual(
      (withData["error"] as Record<string, unknown>)["data"],
      { x: 1 },
    );
  });

  it("buildRequest / buildNotification", () => {
    assert.deepEqual(buildRequest("m", { a: 1 }, "p1"), {
      jsonrpc: "2.0",
      id: "p1",
      method: "m",
      params: { a: 1 },
    });
    assert.deepEqual(buildNotification("m", null), {
      jsonrpc: "2.0",
      method: "m",
      params: null,
    });
  });

  it("error codes match JSON-RPC", () => {
    assert.equal(ERR_PARSE, -32700);
    assert.equal(ERR_INVALID_REQUEST, -32600);
    assert.equal(ERR_METHOD_NOT_FOUND, -32601);
    assert.equal(ERR_INVALID_PARAMS, -32602);
    assert.equal(ERR_INTERNAL, -32603);
  });
});

describe("writeMessage", () => {
  it("writes one JSON line", () => {
    const out = new PassThrough();
    let buf = "";
    out.on("data", (c: unknown) => {
      buf += String(c);
    });
    writeMessage({ a: 1 }, out);
    assert.equal(buf, '{"a":1}\n');
  });
});
