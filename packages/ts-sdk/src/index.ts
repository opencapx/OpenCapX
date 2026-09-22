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

export { Plugin } from "./plugin.js";
export type { Handler, PluginOptions } from "./plugin.js";
export { MethodNotFoundError } from "./plugin.js";
export {
  ERR_PARSE,
  ERR_INVALID_REQUEST,
  ERR_METHOD_NOT_FOUND,
  ERR_INVALID_PARAMS,
  ERR_INTERNAL,
  ProtocolError,
  decodeLine,
  writeMessage,
  buildResult,
  buildError,
  buildRequest,
  buildNotification,
} from "./protocol.js";
export type { JsonRpcMessage, JsonRpcRequestId, JsonValue } from "./protocol.js";
export {
  ManifestError,
  REQUIRED_TOP,
  ALLOWED_TYPES,
  SUPPORTED_API_VERSION,
  loadManifest,
  validateManifest,
} from "./manifest.js";
export type { Manifest } from "./manifest.js";
