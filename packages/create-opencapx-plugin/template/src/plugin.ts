#!/usr/bin/env node
/** OpenCapX plugin template (TypeScript): minimal capability plugin.
 *
 * Replace `image.analyze` with a built-in capability name (see
 * docs/capability.md), or declare a new domain capability in the manifest.
 * Settings come from the manifest `settings[]` (declarative form); re-read
 * them on every capability call so the settings page applies without a
 * plugin restart.
 */
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { Plugin } from "@opencapx/sdk";

const here = dirname(fileURLToPath(import.meta.url));

class __CLASS__ extends Plugin {
  constructor() {
    super({ manifestPath: join(here, "..", "..", "opencapx-plugin.json") });
    this.capability("image.analyze", (params) => this.analyze(params));
    this.method("core.probe.capability", async () => ({
      ok: true,
      via: "__PKG_NAME__",
    }));
  }

  private async analyze(
    params: Record<string, unknown>,
  ): Promise<Record<string, unknown>> {
    // Re-read per call: the settings page writes config / keychain directly,
    // no plugin restart needed.
    const prefix =
      (await this.configGet("response_prefix", "template")) || "template";
    // Secrets only reveal existence; values never enter results
    // (stored under the `secret:` prefix, backed by the keychain).
    const tokenSet = Boolean(await this.configGet("secret:api_token", ""));

    const description =
      `[${prefix}] got ${String(params["image"] ?? "")}` +
      (tokenSet ? "" : " (api token unset)");

    return { description, text: "", objects: [] };
  }
}

await new __CLASS__().run();
