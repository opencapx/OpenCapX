"use strict";
var __importDefault = (this && this.__importDefault) || function (mod) {
    return (mod && mod.__esModule) ? mod : { "default": mod };
};
Object.defineProperty(exports, "__esModule", { value: true });
const node_test_1 = require("node:test");
const strict_1 = __importDefault(require("node:assert/strict"));
const node_fs_1 = require("node:fs");
const node_os_1 = require("node:os");
const node_path_1 = require("node:path");
const manifest_js_1 = require("../src/manifest.js");
function writeTmp(obj) {
    const dir = (0, node_fs_1.mkdtempSync)((0, node_path_1.join)((0, node_os_1.tmpdir)(), "opencapx-ts-manifest-"));
    const p = (0, node_path_1.join)(dir, "opencapx-plugin.json");
    (0, node_fs_1.writeFileSync)(p, typeof obj === "string" ? obj : JSON.stringify(obj));
    return p;
}
const base = {
    id: "com.example.x",
    name: "X",
    version: "0.1.0",
    apiVersion: "1",
    type: "capability",
    capabilities: ["image.analyze"],
    runtime: { type: "process", command: "node", args: ["dist/plugin.js"] },
};
(0, node_test_1.describe)("validateManifest", () => {
    (0, node_test_1.it)("accepts a good manifest and returns it", () => {
        strict_1.default.deepEqual((0, manifest_js_1.validateManifest)({ ...base }), { ...base });
    });
    (0, node_test_1.it)("rejects missing required fields", () => {
        for (const key of ["id", "name", "version", "apiVersion", "type"]) {
            const bad = { ...base };
            delete bad[key];
            strict_1.default.throws(() => (0, manifest_js_1.validateManifest)(bad), manifest_js_1.ManifestError);
        }
    });
    (0, node_test_1.it)("rejects bad type and apiVersion", () => {
        strict_1.default.throws(() => (0, manifest_js_1.validateManifest)({ ...base, type: "widget" }), manifest_js_1.ManifestError);
        strict_1.default.throws(() => (0, manifest_js_1.validateManifest)({ ...base, apiVersion: "2" }), manifest_js_1.ManifestError);
    });
    (0, node_test_1.it)("capability type needs capabilities[] and runtime", () => {
        strict_1.default.throws(() => (0, manifest_js_1.validateManifest)({ ...base, capabilities: [] }), manifest_js_1.ManifestError);
        const noRuntime = { ...base };
        delete noRuntime["runtime"];
        strict_1.default.throws(() => (0, manifest_js_1.validateManifest)(noRuntime), manifest_js_1.ManifestError);
    });
    (0, node_test_1.it)("validates object-form timeoutSecs", () => {
        const ok = {
            ...base,
            capabilities: [{ id: "image.analyze", timeoutSecs: 60 }],
        };
        strict_1.default.deepEqual((0, manifest_js_1.validateManifest)(ok), ok);
        for (const bad of [0, 601, 1.5, true, "60"]) {
            strict_1.default.throws(() => (0, manifest_js_1.validateManifest)({
                ...base,
                capabilities: [{ id: "image.analyze", timeoutSecs: bad }],
            }), manifest_js_1.ManifestError);
        }
    });
});
(0, node_test_1.describe)("loadManifest", () => {
    (0, node_test_1.it)("loads from disk", () => {
        strict_1.default.deepEqual((0, manifest_js_1.loadManifest)(writeTmp(base)), base);
    });
    (0, node_test_1.it)("throws on missing file and bad json", () => {
        strict_1.default.throws(() => (0, manifest_js_1.loadManifest)("/definitely/not/here.json"), manifest_js_1.ManifestError);
        strict_1.default.throws(() => (0, manifest_js_1.loadManifest)(writeTmp("{ nope")), manifest_js_1.ManifestError);
    });
});
