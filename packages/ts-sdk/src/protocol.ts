/** JSON-RPC 2.0 over stdio framing for OpenCapX plugins.
 *
 * One JSON object per line, UTF-8, `\n` separated.
 * stdout carries protocol only; stderr is for logs.
 *
 * Mirrors `packages/plugin-sdk/opencapx_sdk/protocol.py`.
 * See `docs/plugin-protocol.md`.
 */

/** JSON-RPC standard error codes. */
export const ERR_PARSE = -32700;
export const ERR_INVALID_REQUEST = -32600;
export const ERR_METHOD_NOT_FOUND = -32601;
export const ERR_INVALID_PARAMS = -32602;
export const ERR_INTERNAL = -32603;

/** Thrown when a JSON-RPC frame/field is illegal. */
export class ProtocolError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ProtocolError";
  }
}

/** Any JSON value. */
export type JsonValue =
  | null
  | boolean
  | number
  | string
  | JsonValue[]
  | { [key: string]: JsonValue };

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
export function writeMessage(
  msg: unknown,
  out?: NodeJS.WritableStream,
): void {
  const stream: NodeJS.WritableStream = out ?? process.stdout;
  stream.write(`${JSON.stringify(msg)}\n`);
}

/**
 * Decode a single input line. Returns `null` for blank/whitespace-only
 * lines (skip). Throws `ProtocolError` on malformed JSON or non-object
 * payloads.
 */
export function decodeLine(line: string): JsonRpcMessage | null {
  const stripped = line.trim();
  if (!stripped) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(stripped);
  } catch (err) {
    const reason = err instanceof Error ? err.message : String(err);
    throw new ProtocolError(
      `malformed json: ${reason} (line: ${JSON.stringify(stripped.slice(0, 80))})`,
    );
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    throw new ProtocolError(
      `malformed json: top-level must be an object (line: ${JSON.stringify(stripped.slice(0, 80))})`,
    );
  }
  return parsed as JsonRpcMessage;
}

export function buildResult(
  reqId: JsonRpcRequestId,
  result: unknown,
): Record<string, unknown> {
  return { jsonrpc: "2.0", id: reqId, result };
}

export function buildError(
  reqId: JsonRpcRequestId,
  code: number,
  message: string,
  data?: unknown,
): Record<string, unknown> {
  const err: Record<string, unknown> = { code, message };
  if (data !== undefined) err["data"] = data;
  return { jsonrpc: "2.0", id: reqId, error: err };
}

export function buildRequest(
  method: string,
  params: unknown,
  reqId: JsonRpcRequestId,
): Record<string, unknown> {
  return { jsonrpc: "2.0", id: reqId, method, params };
}

export function buildNotification(
  method: string,
  params: unknown,
): Record<string, unknown> {
  return { jsonrpc: "2.0", method, params };
}
