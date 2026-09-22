/** JSON-RPC 2.0 over stdio framing for OpenCapX plugins.
 *
 * One JSON object per line, UTF-8, `\n` separated.
 * stdout carries protocol only; stderr is for logs.
 *
 * Mirrors `packages/plugin-sdk/opencapx_sdk/protocol.py`.
 * See `docs/plugin-protocol.md`.
 */
/** JSON-RPC standard error codes. */
export declare const ERR_PARSE = -32700;
export declare const ERR_INVALID_REQUEST = -32600;
export declare const ERR_METHOD_NOT_FOUND = -32601;
export declare const ERR_INVALID_PARAMS = -32602;
export declare const ERR_INTERNAL = -32603;
/** Thrown when a JSON-RPC frame/field is illegal. */
export declare class ProtocolError extends Error {
    constructor(message: string);
}
/** Any JSON value. */
export type JsonValue = null | boolean | number | string | JsonValue[] | {
    [key: string]: JsonValue;
};
/** A decoded JSON-RPC message envelope. */
export interface JsonRpcMessage {
    jsonrpc?: unknown;
    id?: string | number | null;
    method?: unknown;
    params?: unknown;
    result?: unknown;
    error?: unknown;
}
export type JsonRpcRequestId = string | number;
/** Serialize + write one JSON line to stdout (default) or the given stream. */
export declare function writeMessage(msg: unknown, out?: NodeJS.WritableStream): void;
/**
 * Decode a single input line. Returns `null` for blank/whitespace-only
 * lines (skip). Throws `ProtocolError` on malformed JSON or non-object
 * payloads.
 */
export declare function decodeLine(line: string): JsonRpcMessage | null;
export declare function buildResult(reqId: JsonRpcRequestId, result: unknown): Record<string, unknown>;
export declare function buildError(reqId: JsonRpcRequestId, code: number, message: string, data?: unknown): Record<string, unknown>;
export declare function buildRequest(method: string, params: unknown, reqId: JsonRpcRequestId): Record<string, unknown>;
export declare function buildNotification(method: string, params: unknown): Record<string, unknown>;
