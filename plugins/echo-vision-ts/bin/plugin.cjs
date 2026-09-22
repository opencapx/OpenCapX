#!/usr/bin/env node
/** OpenCapX TypeScript example plugin (same shape as plugin-template).
 *
 * Requires the SDK from this repo checkout (dist is committed) so the
 * fixture installs offline; published plugins use a regular
 * `@opencapx/sdk` dependency. See packages/ts-sdk/README.md.
 */
const path = require("node:path");
const {
  Plugin,
} = require(
  path.join(
    __dirname,
    "..",
    "..",
    "..",
    "packages",
    "ts-sdk",
    "dist",
    "src",
    "index.js",
  ),
);

class EchoVisionTs extends Plugin {
  constructor() {
    super({
      manifestPath: path.join(__dirname, "..", "opencapx-plugin.json"),
    });
    this.capability("image.analyze", async (params) => {
      // Reverse config.get through the real core: unset key → the
      // default we passed, proving the reverse roundtrip end to end.
      const prefix = await this.configGet("response_prefix", "ts-echo");
      return {
        description: `[${prefix}] got ${String(params["image"] ?? "")}`,
        text: "",
        objects: [],
      };
    });
    this.method("core.probe.capability", async () => ({
      ok: true,
      via: "ts",
    }));
  }
}

new EchoVisionTs().run().then((code) => process.exit(code));
