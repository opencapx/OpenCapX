/** OpenCapX Plugin base class: owns the JSON-RPC loop + reverse calls.
 *
 * Mirrors `packages/plugin-sdk/opencapx_sdk/plugin.py`.
 *
 * Minimal usage:
 * ```ts
 * import { Plugin } from "@opencapx/sdk";
 *
 * class EchoVision extends Plugin {
 *   constructor() {
 *     super({ manifestPath: "opencapx-plugin.json" });
 *     this.capability("image.analyze", async (params) => ({
 *       description: `[echo] ${String(params["image"] ?? "")}`,
 *     }));
 *   }
 * }
 *
 * await new EchoVision().run();
 * ```
 *
 * Differences from the Python SDK (all deliberate):
 * - Reverse calls are `async` (Python blocks the loop thread). Await them.
 * - Registration is explicit (`this.capability(...)` / `this.method(...)`)
 *   instead of decorators, so no `experimentalDecorators` tsconfig flag is
 *   needed. Behavior is identical.
 * - Reverse replies are multiplexed through a pending map, so an unrelated
 *   inbound message can never be mistaken for your reply (the Python base
 *   class documents this race and assumes a single thread).
 * - Unknown methods fall through to `onPluginCall`, which throws
 *   `MethodNotFoundError` by default (→ `-32601`, same as Python).
 * - Timeouts resolve with the safe default (permission → `false`,
 *   configGet → `default`, configSet → `false`) instead of hanging.
 */

import { createInterface } from "node:readline";
import {
  ERR_INTERNAL,
  ERR_INVALID_REQUEST,
  ERR_METHOD_NOT_FOUND,
  JsonRpcMessage,
  JsonRpcRequestId,
  buildError,
  buildNotification,
  buildRequest,
  buildResult,
  decodeLine,
  writeMessage,
} from "./protocol.js";
import { Manifest, loadManifest } from "./manifest.js";

/** Capability / method handler: params in, result (or promise) out. */
export type Handler = (
  params: Record<string, unknown>,
) => unknown | Promise<unknown>;

/** Thrown by the default `onPluginCall` (→ `-32601`). */
export class MethodNotFoundError extends Error {
  constructor(method: string) {
    super(`method not found: ${method}`);
    this.name = "MethodNotFoundError";
  }
}

export interface PluginOptions {
  manifestPath?: string;
  manifest?: Manifest;
  coreVersion?: string;
  pluginId?: string;
  /** Override stdio transport (tests). Defaults to process.stdin/stdout. */
  input?: NodeJS.ReadableStream;
  output?: NodeJS.WritableStream;
}

interface PendingReply {
  resolve: (value: unknown) => void;
  timer: ReturnType<typeof setTimeout>;
  fallback: unknown;
}

export class Plugin {
  protected manifest: Manifest;
  readonly pluginId: string;
  readonly coreVersion: string;
  /** Version of the last successful start (injected via `previousVersion`). */
  previousVersion: string | null = null;

  private nextId = 1;
  private readonly capabilities = new Map<string, Handler>();
  private readonly methods = new Map<string, Handler>();
  private readonly pending = new Map<JsonRpcRequestId, PendingReply>();
  private readonly input: NodeJS.ReadableStream;
  private readonly output: NodeJS.WritableStream;
  // One background pump feeds run() and reverse calls; awaiting a reply
  // must never block the reader (read-dispatch deadlock).
  private readonly lineQueue: string[] = [];
  private lineWaiters: Array<() => void> = [];
  private inputClosed = false;
  private pumpStarted = false;
  private lineReader: { close(): void } | null = null;

  constructor(opts: PluginOptions = {}) {
    if (opts.manifest !== undefined) {
      this.manifest = opts.manifest;
    } else if (opts.manifestPath !== undefined) {
      this.manifest = loadManifest(opts.manifestPath);
    } else {
      // Deferred: some plugins (like pet-blank) need no strong manifest.
      this.manifest = {
        id: opts.pluginId ?? "unknown",
        version: "0.0.0",
        apiVersion: "1",
      };
    }
    const id = this.manifest["id"];
    this.pluginId =
      typeof id === "string" && id ? id : (opts.pluginId ?? "unknown");
    this.coreVersion = opts.coreVersion ?? "0.0.0";
    this.input = opts.input ?? process.stdin;
    this.output = opts.output ?? process.stdout;
  }

  /** Register a capability handler. Method name must equal the capability id. */
  capability(id: string, handler: Handler): this {
    this.capabilities.set(id, handler);
    return this;
  }

  /** Register an arbitrary RPC method handler (e.g. `core.probe.capability`). */
  method(name: string, handler: Handler): this {
    this.methods.set(name, handler);
    return this;
  }

  // ---------- reverse calls (plugin → core) ----------

  private nextRequestId(): string {
    const id = `p${this.nextId}`;
    this.nextId += 1;
    return id;
  }

  protected send(msg: unknown): void {
    writeMessage(msg, this.output);
  }

  /** Wait for a reverse-call reply; resolve `fallback` on timeout/EOF. */
  private waitForReply<T>(
    reqId: JsonRpcRequestId,
    timeoutMs: number,
    fallback: T,
  ): Promise<T> {
    return new Promise<T>((resolve) => {
      const timer = setTimeout(() => {
        this.settlePending(reqId, fallback);
      }, timeoutMs);
      this.pending.set(reqId, {
        resolve: (v: unknown) => {
          clearTimeout(timer);
          resolve(v as T);
        },
        timer,
        fallback,
      });
      void this.pumpReplies(reqId);
    });
  }

  /** Settle one waiter with a value (clears its timer). */
  private settlePending(reqId: JsonRpcRequestId, value: unknown): void {
    const entry = this.pending.get(reqId);
    if (!entry) return;
    this.pending.delete(reqId);
    clearTimeout(entry.timer);
    entry.resolve(value);
  }

  /** Settle every waiter with its own fallback (EOF/shutdown path). */
  private settleAllPending(): void {
    for (const [id, entry] of [...this.pending]) {
      this.pending.delete(id);
      clearTimeout(entry.timer);
      entry.resolve(entry.fallback);
    }
  }

  /** Start the background line pump (idempotent). */
  private ensurePump(): void {
    if (this.pumpStarted) return;
    this.pumpStarted = true;
    const rl = createInterface({ input: this.input, crlfDelay: Infinity });
    this.lineReader = rl;
    void (async () => {
      try {
        for await (const line of rl) {
          this.lineQueue.push(line);
          this.flushWaiters();
        }
      } finally {
        rl.close();
        if (this.lineReader === rl) this.lineReader = null;
        this.inputClosed = true;
        this.flushWaiters();
      }
    })();
  }

  private flushWaiters(): void {
    const waiters = this.lineWaiters;
    this.lineWaiters = [];
    for (const fn of waiters) fn();
  }

  /** Next stdin line, or `null` on EOF. Shared by run() and reverse calls. */
  private nextLine(): Promise<string | null> {
    this.ensurePump();
    const queued = this.lineQueue.shift();
    if (queued !== undefined) return Promise.resolve(queued);
    if (this.inputClosed) return Promise.resolve(null);
    return new Promise((resolve) => {
      const waiter = (): void => {
        const next = this.lineQueue.shift();
        if (next !== undefined) resolve(next);
        else if (this.inputClosed) resolve(null);
        else this.lineWaiters.push(waiter);
      };
      this.lineWaiters.push(waiter);
    });
  }

  /**
   * Feed lines to one reverse-call waiter until it settles. Every consumed
   * line is classified BEFORE this pump decides to exit: a line may be the
   * reply of another waiter (whose pump we might be standing in for), so a
   * pump whose own request already settled must hand lines off — settle the
   * other waiter's reply / dispatch the inbound request — never drop them.
   * Inbound core requests arriving mid-wait are answered inline; replies for
   * unknown ids (late/timeout strays) are the only lines dropped.
   */
  private async pumpReplies(reqId: JsonRpcRequestId): Promise<void> {
    for (;;) {
      const line = await this.nextLine();
      if (line === null) {
        const entry = this.pending.get(reqId);
        if (entry) this.settlePending(reqId, entry.fallback);
        return;
      }
      let msg: JsonRpcMessage;
      try {
        const decoded = decodeLine(line);
        if (decoded === null) continue;
        msg = decoded;
      } catch {
        continue;
      }
      const mid = msg.id;
      if (
        (msg.method === undefined || msg.method === null) &&
        mid !== undefined &&
        mid !== null &&
        this.pending.has(mid as JsonRpcRequestId)
      ) {
        this.settlePending(mid as JsonRpcRequestId, msg);
      } else if (typeof msg.method === "string" && msg.method) {
        const resp = await this.handle(msg);
        if (resp !== null) this.send(resp);
      }
      // Exit only AFTER the current line has been handed off: our own
      // waiter may have settled while we were blocked on nextLine().
      if (!this.pending.has(reqId)) return;
    }
  }

  /** `core.log(level, message)` notification (no reply expected). */
  async log(level: string, message: string): Promise<void> {
    this.send(buildNotification("core.log", { level, message }));
  }

  /** `core.emit({type, payload})` notification. Prefix kind with plugin id. */
  async emit(kind: string, payload: unknown): Promise<void> {
    this.send(buildNotification("core.emit", { type: kind, payload }));
  }

  /**
   * `core.requestPermission(permission, reason)` — resolves `true` iff core
   * replies `{granted: true}`. Timeout/EOF resolve `false`.
   */
  async requestPermission(
    permission: string,
    reason = "",
    timeoutMs = 65_000,
  ): Promise<boolean> {
    const reqId = this.nextRequestId();
    const params: Record<string, string> = { permission };
    if (reason) params["reason"] = reason;
    this.send(buildRequest("core.requestPermission", params, reqId));
    const reply = await this.waitForReply<unknown>(reqId, timeoutMs, null);
    if (
      typeof reply === "object" &&
      reply !== null &&
      "result" in reply &&
      typeof (reply as { result?: unknown }).result === "object" &&
      (reply as { result?: unknown }).result !== null
    ) {
      return Boolean(
        ((reply as { result: Record<string, unknown> }).result["granted"]),
      );
    }
    return false;
  }

  /**
   * Reverse `config.get {key, default}`.
   *
   * Core replies with the **bare** stored value (`{"result": <value>}`),
   * not a `{"value": ...}` envelope. Pass `key="secret:<name>"` to read a
   * keychain-backed secret back (owner-only plaintext).
   */
  async configGet<T>(key: string, def?: T, timeoutMs = 30_000): Promise<T> {
    const reqId = this.nextRequestId();
    this.send(buildRequest("config.get", { key, default: def }, reqId));
    const reply = await this.waitForReply<unknown>(reqId, timeoutMs, null);
    if (
      typeof reply === "object" &&
      reply !== null &&
      "result" in reply
    ) {
      return (reply as { result: T }).result;
    }
    return def as T;
  }

  /** Reverse `config.set {key, value}` — resolves `true` on acknowledgement. */
  async configSet(key: string, value: unknown, timeoutMs = 30_000): Promise<boolean> {
    const reqId = this.nextRequestId();
    this.send(buildRequest("config.set", { key, value }, reqId));
    const reply = await this.waitForReply<unknown>(reqId, timeoutMs, null);
    return (
      typeof reply === "object" &&
      reply !== null &&
      (reply as { id?: unknown }).id === reqId &&
      "result" in reply
    );
  }

  // ---------- lifecycle hooks (override in subclass) ----------

  protected async onInitialize(
    params: Record<string, unknown>,
  ): Promise<Record<string, unknown>> {
    const prev = params["previousVersion"];
    this.previousVersion = typeof prev === "string" ? prev : null;
    // The registered capabilities must be returned here — Core registers
    // the plugin into the Capability Registry from this handshake.
    // `version` is the manifest default "1" (a string, not semver).
    const version = this.manifest["version"];
    const apiVersion = this.manifest["apiVersion"];
    return {
      pluginId: this.pluginId,
      version: typeof version === "string" ? version : "0.0.0",
      apiVersion: typeof apiVersion === "string" ? apiVersion : "1",
      capabilities: [...this.capabilities.keys()]
        .sort()
        .map((id) => ({ id, version: "1" })),
    };
  }

  protected async onShutdown(
    _params: Record<string, unknown>,
  ): Promise<null> {
    return null;
  }

  protected async onPing(
    _params: Record<string, unknown>,
  ): Promise<Record<string, unknown>> {
    return { ok: true };
  }

  /** Fallback for unregistered methods. Throws `MethodNotFoundError` by default. */
  protected async onPluginCall(
    method: string,
    _params: Record<string, unknown>,
  ): Promise<unknown> {
    throw new MethodNotFoundError(method);
  }

  // ---------- dispatch ----------

  private async dispatch(
    method: string,
    params: Record<string, unknown>,
  ): Promise<unknown> {
    if (method === "plugin.initialize") return this.onInitialize(params);
    if (method === "plugin.shutdown") {
      await this.onShutdown(params);
      return null;
    }
    if (method === "plugin.ping") return this.onPing(params);
    const cap = this.capabilities.get(method);
    if (cap) return cap(params);
    const fn = this.methods.get(method);
    if (fn) return fn(params);
    return this.onPluginCall(method, params);
  }

  /**
   * Handle a single JSON-RPC message. Returns the response, or `null` for
   * notifications. Unit tests can call this directly with mocked transport.
   */
  async handle(msg: JsonRpcMessage): Promise<Record<string, unknown> | null> {
    if (msg.jsonrpc !== "2.0") {
      return buildError(
        msg.id as JsonRpcRequestId,
        ERR_INVALID_REQUEST,
        "jsonrpc must be 2.0",
      );
    }
    const method = msg.method;
    const rawParams = msg.params;
    const params: Record<string, unknown> =
      typeof rawParams === "object" && rawParams !== null && !Array.isArray(rawParams)
        ? (rawParams as Record<string, unknown>)
        : {};
    const reqId = msg.id ?? null;
    if (typeof method !== "string" || !method) {
      if (reqId !== null && reqId !== undefined) {
        return buildError(reqId, ERR_INVALID_REQUEST, "missing method");
      }
      return null;
    }
    try {
      const result = await this.dispatch(method, params);
      if (reqId === null || reqId === undefined) return null; // notification
      return buildResult(reqId, result);
    } catch (err) {
      if (reqId === null || reqId === undefined) return null;
      if (err instanceof MethodNotFoundError) {
        return buildError(reqId, ERR_METHOD_NOT_FOUND, err.message);
      }
      const name = err instanceof Error ? err.constructor.name : "Error";
      const message = err instanceof Error ? err.message : String(err);
      return buildError(reqId, ERR_INTERNAL, `${name}: ${message}`);
    }
  }

  /** Main loop: read JSON-RPC from stdin, handle, write stdout. Exit code 0. */
  async run(): Promise<number> {
    for (;;) {
      const line = await this.nextLine();
      if (line === null) {
        this.settleAllPending();
        return 0;
      }
      let msg: JsonRpcMessage | null;
      try {
        msg = decodeLine(line);
      } catch {
        continue; // skip malformed lines
      }
      if (msg === null) continue; // blank line
      // Reverse-call reply? Settle the waiter, never dispatch it.
      if (
        (msg.method === undefined || msg.method === null) &&
        msg.id !== undefined &&
        msg.id !== null &&
        this.pending.has(msg.id as JsonRpcRequestId)
      ) {
        this.settlePending(msg.id as JsonRpcRequestId, msg);
        continue;
      }
      if (msg.method === "plugin.shutdown") {
        await this.handle(msg);
        this.settleAllPending();
        this.lineReader?.close();
        return 0;
      }
      const resp = await this.handle(msg);
      if (resp !== null) this.send(resp);
    }
  }
}
