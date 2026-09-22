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
import { JsonRpcMessage } from "./protocol.js";
import { Manifest } from "./manifest.js";
/** Capability / method handler: params in, result (or promise) out. */
export type Handler = (params: Record<string, unknown>) => unknown | Promise<unknown>;
/** Thrown by the default `onPluginCall` (→ `-32601`). */
export declare class MethodNotFoundError extends Error {
    constructor(method: string);
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
export declare class Plugin {
    protected manifest: Manifest;
    readonly pluginId: string;
    readonly coreVersion: string;
    /** Version of the last successful start (injected via `previousVersion`). */
    previousVersion: string | null;
    private nextId;
    private readonly capabilities;
    private readonly methods;
    private readonly pending;
    private readonly input;
    private readonly output;
    private readonly lineQueue;
    private lineWaiters;
    private inputClosed;
    private pumpStarted;
    private lineReader;
    constructor(opts?: PluginOptions);
    /** Register a capability handler. Method name must equal the capability id. */
    capability(id: string, handler: Handler): this;
    /** Register an arbitrary RPC method handler (e.g. `core.probe.capability`). */
    method(name: string, handler: Handler): this;
    private nextRequestId;
    protected send(msg: unknown): void;
    /** Wait for a reverse-call reply; resolve `fallback` on timeout/EOF. */
    private waitForReply;
    /** Settle one waiter with a value (clears its timer). */
    private settlePending;
    /** Settle every waiter with its own fallback (EOF/shutdown path). */
    private settleAllPending;
    /** Start the background line pump (idempotent). */
    private ensurePump;
    private flushWaiters;
    /** Next stdin line, or `null` on EOF. Shared by run() and reverse calls. */
    private nextLine;
    /**
     * Feed lines to one reverse-call waiter until it settles. Every consumed
     * line is classified BEFORE this pump decides to exit: a line may be the
     * reply of another waiter (whose pump we might be standing in for), so a
     * pump whose own request already settled must hand lines off — settle the
     * other waiter's reply / dispatch the inbound request — never drop them.
     * Inbound core requests arriving mid-wait are answered inline; replies for
     * unknown ids (late/timeout strays) are the only lines dropped.
     */
    private pumpReplies;
    /** `core.log(level, message)` notification (no reply expected). */
    log(level: string, message: string): Promise<void>;
    /** `core.emit({type, payload})` notification. Prefix kind with plugin id. */
    emit(kind: string, payload: unknown): Promise<void>;
    /**
     * `core.requestPermission(permission, reason)` — resolves `true` iff core
     * replies `{granted: true}`. Timeout/EOF resolve `false`.
     */
    requestPermission(permission: string, reason?: string, timeoutMs?: number): Promise<boolean>;
    /**
     * Reverse `config.get {key, default}`.
     *
     * Core replies with the **bare** stored value (`{"result": <value>}`),
     * not a `{"value": ...}` envelope. Pass `key="secret:<name>"` to read a
     * keychain-backed secret back (owner-only plaintext).
     */
    configGet<T>(key: string, def?: T, timeoutMs?: number): Promise<T>;
    /** Reverse `config.set {key, value}` — resolves `true` on acknowledgement. */
    configSet(key: string, value: unknown, timeoutMs?: number): Promise<boolean>;
    protected onInitialize(params: Record<string, unknown>): Promise<Record<string, unknown>>;
    protected onShutdown(_params: Record<string, unknown>): Promise<null>;
    protected onPing(_params: Record<string, unknown>): Promise<Record<string, unknown>>;
    /** Fallback for unregistered methods. Throws `MethodNotFoundError` by default. */
    protected onPluginCall(method: string, _params: Record<string, unknown>): Promise<unknown>;
    private dispatch;
    /**
     * Handle a single JSON-RPC message. Returns the response, or `null` for
     * notifications. Unit tests can call this directly with mocked transport.
     */
    handle(msg: JsonRpcMessage): Promise<Record<string, unknown> | null>;
    /** Main loop: read JSON-RPC from stdin, handle, write stdout. Exit code 0. */
    run(): Promise<number>;
}
