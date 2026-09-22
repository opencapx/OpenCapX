"use strict";
/** OpenCapX plugin SDK for TypeScript (JSON-RPC 2.0 over stdio).
 *
 * Public API:
 *     Plugin               — base class with stdio loop + lifecycle hooks
 *     Handler              — capability/method handler signature
 *     MethodNotFoundError  — default onPluginCall failure (→ -32601)
 *     readMessage helpers  — decodeLine / writeMessage + builders
 *     loadManifest         — parse + validate opencapx-plugin.json
 *     ManifestError        — manifest parse/validate error
 *     ProtocolError        — JSON-RPC framing error
 *     ERR_*                — JSON-RPC standard error codes
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.validateManifest = exports.loadManifest = exports.SUPPORTED_API_VERSION = exports.ALLOWED_TYPES = exports.REQUIRED_TOP = exports.ManifestError = exports.buildNotification = exports.buildRequest = exports.buildError = exports.buildResult = exports.writeMessage = exports.decodeLine = exports.ProtocolError = exports.ERR_INTERNAL = exports.ERR_INVALID_PARAMS = exports.ERR_METHOD_NOT_FOUND = exports.ERR_INVALID_REQUEST = exports.ERR_PARSE = exports.MethodNotFoundError = exports.Plugin = void 0;
var plugin_js_1 = require("./plugin.js");
Object.defineProperty(exports, "Plugin", { enumerable: true, get: function () { return plugin_js_1.Plugin; } });
var plugin_js_2 = require("./plugin.js");
Object.defineProperty(exports, "MethodNotFoundError", { enumerable: true, get: function () { return plugin_js_2.MethodNotFoundError; } });
var protocol_js_1 = require("./protocol.js");
Object.defineProperty(exports, "ERR_PARSE", { enumerable: true, get: function () { return protocol_js_1.ERR_PARSE; } });
Object.defineProperty(exports, "ERR_INVALID_REQUEST", { enumerable: true, get: function () { return protocol_js_1.ERR_INVALID_REQUEST; } });
Object.defineProperty(exports, "ERR_METHOD_NOT_FOUND", { enumerable: true, get: function () { return protocol_js_1.ERR_METHOD_NOT_FOUND; } });
Object.defineProperty(exports, "ERR_INVALID_PARAMS", { enumerable: true, get: function () { return protocol_js_1.ERR_INVALID_PARAMS; } });
Object.defineProperty(exports, "ERR_INTERNAL", { enumerable: true, get: function () { return protocol_js_1.ERR_INTERNAL; } });
Object.defineProperty(exports, "ProtocolError", { enumerable: true, get: function () { return protocol_js_1.ProtocolError; } });
Object.defineProperty(exports, "decodeLine", { enumerable: true, get: function () { return protocol_js_1.decodeLine; } });
Object.defineProperty(exports, "writeMessage", { enumerable: true, get: function () { return protocol_js_1.writeMessage; } });
Object.defineProperty(exports, "buildResult", { enumerable: true, get: function () { return protocol_js_1.buildResult; } });
Object.defineProperty(exports, "buildError", { enumerable: true, get: function () { return protocol_js_1.buildError; } });
Object.defineProperty(exports, "buildRequest", { enumerable: true, get: function () { return protocol_js_1.buildRequest; } });
Object.defineProperty(exports, "buildNotification", { enumerable: true, get: function () { return protocol_js_1.buildNotification; } });
var manifest_js_1 = require("./manifest.js");
Object.defineProperty(exports, "ManifestError", { enumerable: true, get: function () { return manifest_js_1.ManifestError; } });
Object.defineProperty(exports, "REQUIRED_TOP", { enumerable: true, get: function () { return manifest_js_1.REQUIRED_TOP; } });
Object.defineProperty(exports, "ALLOWED_TYPES", { enumerable: true, get: function () { return manifest_js_1.ALLOWED_TYPES; } });
Object.defineProperty(exports, "SUPPORTED_API_VERSION", { enumerable: true, get: function () { return manifest_js_1.SUPPORTED_API_VERSION; } });
Object.defineProperty(exports, "loadManifest", { enumerable: true, get: function () { return manifest_js_1.loadManifest; } });
Object.defineProperty(exports, "validateManifest", { enumerable: true, get: function () { return manifest_js_1.validateManifest; } });
