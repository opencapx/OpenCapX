import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { Plugin } from "@opencapx/sdk";

const MANIFEST = {
  id: "__ID__",
  name: "__NAME__",
  version: "0.1.0",
  apiVersion: "1",
  type: "capability",
  capabilities: ["image.analyze"],
  runtime: { type: "process", command: "node", args: ["dist/src/plugin.js"] },
};

class TestPlugin extends Plugin {
  constructor() {
    super({ manifest: MANIFEST });
    this.capability("image.analyze", async (params) => ({
      description: `[template] got ${String(params["image"] ?? "")}`,
    }));
  }
}

describe("template plugin", () => {
  it("answers image.analyze", async () => {
    const p = new TestPlugin();
    const resp = await p.handle({
      jsonrpc: "2.0",
      id: 1,
      method: "image.analyze",
      params: { image: "shot.png" },
    });
    assert.deepEqual(resp, {
      jsonrpc: "2.0",
      id: 1,
      result: { description: "[template] got shot.png" },
    });
  });

  it("reports capabilities in the handshake", async () => {
    const p = new TestPlugin();
    const resp = await p.handle({
      jsonrpc: "2.0",
      id: 2,
      method: "plugin.initialize",
      params: {},
    });
    assert.deepEqual((resp as Record<string, unknown>)["result"], {
      pluginId: "__ID__",
      version: "0.1.0",
      apiVersion: "1",
      capabilities: [{ id: "image.analyze", version: "1" }],
    });
  });
});
