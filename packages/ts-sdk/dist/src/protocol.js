"use strict";
/** JSON-RPC 2.0 over stdio framing for OpenCapX plugins.
 *
 * One JSON object per line, UTF-8, `\n` separated.
 * stdout carries protocol only; stderr is for logs.
 *
 * Mirrors `packages/plugin-sdk/opencapx_sdk/protocol.py`.
 * See `docs/plugin-protocol.md`.
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.ProtocolError = exports.ERR_INTERNAL = exports.ERR_INVALID_PARAMS = exports.ERR_METHOD_NOT_FOUND = exports.ERR_INVALID_REQUEST = exports.ERR_PARSE = void 0;
exports.writeMessage = writeMessage;
exports.decodeLine = decodeLine;
exports.buildResult = buildResult;
exports.buildError = buildError;
exports.buildRequest = buildRequest;
exports.buildNotification = buildNotification;
/** JSON-RPC standard error codes. */
exports.ERR_PARSE = -32700;
exports.ERR_INVALID_REQUEST = -32600;
exports.ERR_METHOD_NOT_FOUND = -32601;
exports.ERR_INVALID_PARAMS = -32602;
exports.ERR_INTERNAL = -32603;
/** Thrown when a JSON-RPC frame/field is illegal. */
class ProtocolError extends Error {
    constructor(message) {
        super(message);
        this.name = "ProtocolError";
    }
}
exports.ProtocolError = ProtocolError;
/** Serialize + write one JSON line to stdout (default) or the given stream. */
function writeMessage(msg, out) {
    const stream = out ?? process.stdout;
    stream.write(`${JSON.stringify(msg)}\n`);
}
/**
 * Decode a single input line. Returns `null` for blank/whitespace-only
 * lines (skip). Throws `ProtocolError` on malformed JSON or non-object
 * payloads.
 */
function decodeLine(line) {
    const stripped = line.trim();
    if (!stripped)
        return null;
    let parsed;
    try {
        parsed = JSON.parse(stripped);
    }
    catch (err) {
        const reason = err instanceof Error ? err.message : String(err);
        throw new ProtocolError(`malformed json: ${reason} (line: ${JSON.stringify(stripped.slice(0, 80))})`);
    }
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
        throw new ProtocolError(`malformed json: top-level must be an object (line: ${JSON.stringify(stripped.slice(0, 80))})`);
    }
    return parsed;
}
function buildResult(reqId, result) {
    return { jsonrpc: "2.0", id: reqId, result };
}
function buildError(reqId, code, message, data) {
    const err = { code, message };
    if (data !== undefined)
        err["data"] = data;
    return { jsonrpc: "2.0", id: reqId, error: err };
}
function buildRequest(method, params, reqId) {
    return { jsonrpc: "2.0", id: reqId, method, params };
}
function buildNotification(method, params) {
    return { jsonrpc: "2.0", method, params };
}
