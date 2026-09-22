import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { PassThrough } from "node:stream";
import { Plugin } from "../src/plugin.js";

const MANIFEST = {
  id: "com.example.echo",
  name: "Echo",
  version: "0.2.0",
  apiVersion: "1",
  type: "capability",
  capabilities: ["echo"],
  runtime: { type: "process", command: "node", args: ["dist/plugin.js"] },
};

class EchoPlugin extends Plugin {
  constructor(opts: Record<string, unknown> = {}) {
    super({ manifest: MANIFEST, ...opts } as ConstructorParameters<typeof Plugin>[0]);
    this.capability("echo", async (params) => params);
    this.method("core.probe.capability", async () => ({ ok: true }));
  }

  async boom(): Promise<void> {
    this.capability("boom", async () => {
      throw new TypeError("kaput");
    });
  }
}

function makeTransport() {
  const input = new PassThrough();
  const output = new PassThrough();
  const lines: string[] = [];
  output.on("data", (c: unknown) => {
    for (const part of String(c).split("\n")) {
      if (part) lines.push(part);
    }
  });
  return { input, output, lines };
}

/** Wait until `lines` holds more than `n` entries (request flushed). */
async function waitForLines(
  lines: string[],
  n: number,
): Promise<void> {
  for (let i = 0; i < 200 && lines.length <= n; i++) {
    await new Promise((r) => setImmediate(r));
  }
  assert.ok(lines.length > n, "expected plugin to write a request");
}

describe("handle", () => {
  it("dispatches capability handlers", async () => {
    const p = new EchoPlugin();
    const resp = await p.handle({
      jsonrpc: "2.0",
      id: 1,
      method: "echo",
      params: { hi: 1 },
    });
    assert.deepEqual(resp, {
      jsonrpc: "2.0",
      id: 1,
      result: { hi: 1 },
    });
  });

  it("dispatches @method handlers", async () => {
    const p = new EchoPlugin();
    const resp = await p.handle({
      jsonrpc: "2.0",
      id: 2,
      method: "core.probe.capability",
      params: {},
    });
    assert.deepEqual((resp as Record<string, unknown>)["result"], {
      ok: true,
    });
  });

  it("answers plugin.ping by default", async () => {
    const p = new EchoPlugin();
    const resp = await p.handle({
      jsonrpc: "2.0",
      id: 3,
      method: "plugin.ping",
      params: {},
    });
    assert.deepEqual((resp as Record<string, unknown>)["result"], {
      ok: true,
    });
  });

  it("returns the handshake on plugin.initialize", async () => {
    const p = new EchoPlugin();
    const resp = await p.handle({
      jsonrpc: "2.0",
      id: 4,
      method: "plugin.initialize",
      params: {},
    });
    assert.deepEqual((resp as Record<string, unknown>)["result"], {
      pluginId: "com.example.echo",
      version: "0.2.0",
      apiVersion: "1",
      capabilities: [{ id: "echo", version: "1" }],
    });
    assert.equal(p.previousVersion, null);
  });

  it("records previousVersion from the handshake", async () => {
    const p = new EchoPlugin();
    await p.handle({
      jsonrpc: "2.0",
      id: 5,
      method: "plugin.initialize",
      params: { previousVersion: "0.1.0" },
    });
    assert.equal(p.previousVersion, "0.1.0");
  });

  it("maps unknown methods to -32601", async () => {
    const p = new EchoPlugin();
    const resp = await p.handle({
      jsonrpc: "2.0",
      id: 6,
      method: "nope",
      params: {},
    });
    assert.deepEqual(resp, {
      jsonrpc: "2.0",
      id: 6,
      error: { code: -32601, message: "method not found: nope" },
    });
  });

  it("maps handler exceptions to -32603 with class name", async () => {
    const p = new EchoPlugin();
    await p.boom();
    const resp = await p.handle({
      jsonrpc: "2.0",
      id: 7,
      method: "boom",
      params: {},
    });
    assert.deepEqual(resp, {
      jsonrpc: "2.0",
      id: 7,
      error: { code: -32603, message: "TypeError: kaput" },
    });
  });

  it("returns null for notifications", async () => {
    const p = new EchoPlugin();
    assert.equal(
      await p.handle({ jsonrpc: "2.0", method: "echo", params: {} }),
      null,
    );
  });

  it("rejects non-2.0 envelopes and missing methods", async () => {
    const p = new EchoPlugin();
    const bad = await p.handle({ jsonrpc: "1.0", id: 8, method: "echo" });
    assert.equal(
      ((bad as Record<string, unknown>)["error"] as Record<string, unknown>)["code"],
      -32600,
    );
    const missing = await p.handle({ jsonrpc: "2.0", id: 9 });
    assert.equal(
      ((missing as Record<string, unknown>)["error"] as Record<string, unknown>)["code"],
      -32600,
    );
  });
});

describe("reverse calls", () => {
  it("configGet resolves the bare value", async () => {
    const { input, output, lines } = makeTransport();
    const p = new EchoPlugin({
      manifest: MANIFEST,
      input,
      output,
    });
    const running = p.run();
    const pending = p.configGet("k", "dflt");
    await waitForLines(lines, 0);
    const req = JSON.parse(lines[lines.length - 1]) as { id: string | number };
    input.write(
      `${JSON.stringify({ jsonrpc: "2.0", id: req.id, result: "live" })}\n`,
    );
    assert.equal(await pending, "live");
    input.end();
    assert.equal(await running, 0);
  });

  it("configGet falls back to default on timeout", async () => {
    const { input, output, lines } = makeTransport();
    const p = new EchoPlugin({
      manifest: MANIFEST,
      input,
      output,
    });
    assert.equal(await p.configGet("k", "dflt", 15), "dflt");
    input.end();
  });

  it("resolves two concurrent configGet calls (in-order replies, regression)", async () => {
    const { input, output, lines } = makeTransport();
    const p = new EchoPlugin({
      manifest: MANIFEST,
      input,
      output,
    });
    const running = p.run();
    const both = Promise.all([
      p.configGet("a", "defA", 100),
      p.configGet("b", "defB", 100),
    ]);
    await waitForLines(lines, 1);
    const ids = lines.map((l) => (JSON.parse(l) as { id: string }).id);
    // Reply in request order. A pump whose own request already settled
    // must hand off a consumed reply line to the other waiter, never
    // drop it — dropping makes the second call silently resolve with
    // its default after the timeout.
    for (const id of ids) {
      input.write(
        `${JSON.stringify({ jsonrpc: "2.0", id, result: `val-${id}` })}\n`,
      );
    }
    assert.deepEqual(await both, ids.map((id) => `val-${id}`));
    input.end();
    assert.equal(await running, 0);
  });

  it("resolves two concurrent configGet calls (out-of-order replies, regression)", async () => {
    const { input, output, lines } = makeTransport();
    const p = new EchoPlugin({
      manifest: MANIFEST,
      input,
      output,
    });
    const running = p.run();
    const both = Promise.all([
      p.configGet("a", "defA", 100),
      p.configGet("b", "defB", 100),
    ]);
    await waitForLines(lines, 1);
    const ids = lines.map((l) => (JSON.parse(l) as { id: string }).id);
    input.write(
      `${JSON.stringify({ jsonrpc: "2.0", id: ids[1], result: "second" })}\n`,
    );
    input.write(
      `${JSON.stringify({ jsonrpc: "2.0", id: ids[0], result: "first" })}\n`,
    );
    assert.deepEqual(await both, ["first", "second"]);
    input.end();
    assert.equal(await running, 0);
  });

  it("serves inbound requests between concurrent reverse replies (regression)", async () => {
    const { input, output, lines } = makeTransport();
    const p = new EchoPlugin({
      manifest: MANIFEST,
      input,
      output,
    });
    const running = p.run();
    const both = Promise.all([
      p.configGet("a", "defA", 100),
      p.configGet("b", "defB", 100),
    ]);
    await waitForLines(lines, 1);
    const ids = lines.map((l) => (JSON.parse(l) as { id: string }).id);
    input.write(
      `${JSON.stringify({ jsonrpc: "2.0", id: ids[1], result: "late-b" })}\n`,
    );
    input.write(
      `${JSON.stringify({ jsonrpc: "2.0", id: 99, method: "plugin.ping", params: {} })}\n`,
    );
    input.write(
      `${JSON.stringify({ jsonrpc: "2.0", id: ids[0], result: "late-a" })}\n`,
    );
    assert.deepEqual(await both, ["late-a", "late-b"]);
    await waitForLines(lines, 2);
    assert.ok(
      lines.some((l) => l.includes('"id":99') && l.includes('"ok"')),
      "ping response expected while reverse replies were pending",
    );
    input.end();
    assert.equal(await running, 0);
  });

  it("configSet resolves true on acknowledgement", async () => {
    const { input, output, lines } = makeTransport();
    const p = new EchoPlugin({
      manifest: MANIFEST,
      input,
      output,
    });
    const running = p.run();
    const pending = p.configSet("k", 1);
    await waitForLines(lines, 0);
    const req = JSON.parse(lines[lines.length - 1]) as { id: string | number };
    input.write(
      `${JSON.stringify({ jsonrpc: "2.0", id: req.id, result: {} })}\n`,
    );
    assert.equal(await pending, true);
    input.end();
    assert.equal(await running, 0);
  });

  it("requestPermission maps granted and errors to bool", async () => {
    const t1 = makeTransport();
    const p1 = new EchoPlugin({
      manifest: MANIFEST,
      input: t1.input,
      output: t1.output,
    });
    const running1 = p1.run();
    const pendingYes = p1.requestPermission("image.read");
    await waitForLines(t1.lines, 0);
    const req = JSON.parse(t1.lines[t1.lines.length - 1]) as {
      id: string | number;
    };
    t1.input.write(
      `${JSON.stringify({ jsonrpc: "2.0", id: req.id, result: { granted: true } })}\n`,
    );
    assert.equal(await pendingYes, true);
    t1.input.end();
    assert.equal(await running1, 0);

    const t2 = makeTransport();
    const p2 = new EchoPlugin({
      manifest: MANIFEST,
      input: t2.input,
      output: t2.output,
    });
    const running2 = p2.run();
    const pendingNo = p2.requestPermission("image.read");
    await waitForLines(t2.lines, 0);
    const req2 = JSON.parse(t2.lines[t2.lines.length - 1]) as {
      id: string | number;
    };
    t2.input.write(
      `${JSON.stringify({ jsonrpc: "2.0", id: req2.id, error: { code: -32603, message: "denied" } })}\n`,
    );
    assert.equal(await pendingNo, false);
    t2.input.end();
    assert.equal(await running2, 0);
  });
});
describe("run", () => {
  it("answers initialize and exits 0 on shutdown", async () => {
    const { input, output, lines } = makeTransport();
    const p = new EchoPlugin({ manifest: MANIFEST, input, output });
    const done = p.run();
    input.write(
      `${JSON.stringify({ jsonrpc: "2.0", id: 1, method: "plugin.initialize", params: {} })}\n`,
    );
    input.write(
      `${JSON.stringify({ jsonrpc: "2.0", method: "plugin.shutdown", params: {} })}\n`,
    );
    input.end();
    assert.equal(await done, 0);
    const init = JSON.parse(lines[0]) as Record<string, unknown>;
    assert.deepEqual(init["result"], {
      pluginId: "com.example.echo",
      version: "0.2.0",
      apiVersion: "1",
      capabilities: [{ id: "echo", version: "1" }],
    });
  });

  it("exits 0 on shutdown without stdin closing (regression)", async () => {
    const { input, output } = makeTransport();
    const p = new EchoPlugin({ manifest: MANIFEST, input, output });
    const done = p.run();
    input.write(
      `${JSON.stringify({ jsonrpc: "2.0", method: "plugin.shutdown", params: {} })}\n`,
    );
    assert.equal(await done, 0);
    input.end();
  });
});
