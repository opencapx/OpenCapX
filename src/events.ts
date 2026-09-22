// Frontend SSE client: subscribes to GET /events and dispatches OpencapxEvent to callbacks registered by kind.
// Auto-reconnects (exponential backoff 1s→30s). The Tauri event channel is still the main path; this client serves browsers /
// debug panels / third-party dashboards, and the live stream is also visible in settings' Audit tab.

export interface OpencapxEvent {
  id: string;
  kind: string;
  source: string;
  timestamp: number;
  payload: unknown;
}

export type EventListener = (e: OpencapxEvent) => void;

const listeners = new Map<string, Set<EventListener>>();
let stream: EventSource | null = null;
let retryMs = 1000;
let retryTimer: ReturnType<typeof setTimeout> | null = null;
let lastEventId = "";

export function onEvent(kind: string, cb: EventListener): () => void {
  let set = listeners.get(kind);
  if (!set) {
    set = new Set();
    listeners.set(kind, set);
  }
  set.add(cb);
  return () => {
    set!.delete(cb);
    if (set!.size === 0) listeners.delete(kind);
  };
}

export function emitLocal(e: OpencapxEvent): void {
  const set = listeners.get(e.kind);
  if (set) for (const cb of set) try { cb(e); } catch (err) { console.error("event listener", err); }
  const all = listeners.get("*");
  if (all) for (const cb of all) try { cb(e); } catch (err) { console.error("event listener", err); }
}

/** Parse one SSE data chunk (`event: k\ndata: <json>\n\n` → OpencapxEvent). */
export function parseSseChunk(chunk: string): OpencapxEvent | null {
  let kind = "";
  let data = "";
  let id = "";
  for (const line of chunk.split("\n")) {
    if (line.startsWith(":")) continue; // comment / heartbeat
    const i = line.indexOf(":");
    if (i < 0) continue;
    const field = line.slice(0, i).trim();
    let value = line.slice(i + 1);
    if (value.startsWith(" ")) value = value.slice(1);
    if (field === "event") kind = value;
    else if (field === "data") data += (data ? "\n" : "") + value;
    else if (field === "id") id = value;
  }
  if (!kind || !data) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(data);
  } catch {
    return null;
  }
  if (!parsed || typeof parsed !== "object") return null;
  const obj = parsed as Record<string, unknown>;
  return {
    id: String(obj.id ?? id),
    kind: String(obj.kind ?? kind),
    source: String(obj.source ?? ""),
    timestamp: Number(obj.timestamp ?? 0),
    payload: obj.payload ?? null,
  };
}

function scheduleReconnect(url: string): void {
  if (retryTimer) return;
  const delay = retryMs;
  retryTimer = setTimeout(() => {
    retryTimer = null;
    retryMs = Math.min(retryMs * 2, 30_000);
    open(url);
  }, delay);
}

function open(url: string): void {
  if (typeof EventSource === "undefined") return; // skip in non-browser environments (SSR/test)
  try {
    stream = new EventSource(url);
  } catch {
    scheduleReconnect(url);
    return;
  }
  stream.onopen = () => {
    retryMs = 1000;
  };
  stream.onerror = () => {
    stream?.close();
    stream = null;
    scheduleReconnect(url);
  };
  stream.onmessage = (msg) => {
    if (msg.lastEventId) lastEventId = msg.lastEventId;
    // Default event: message branch — this service uses event: {kind}, but plain message is a fallback too
    const ev = parseSseChunk(`event: message\ndata: ${msg.data}\n`);
    if (ev) emitLocal(ev);
  };
  // EventSource dispatches by event name by default; `event: foo` sent by the tiny_http side triggers addEventListener("foo")
  // We rely on onmessage as a fallback and additionally listen for common event types
  for (const kind of ["permission.ask", "permission.granted", "permission.deny", "pet.bubble", "pet.state", "agent.state", "plugin.metrics.sampled", "plugin.metrics.exceeded", "workspace.switched"]) {
    stream.addEventListener(kind, (msg) => {
      const ev = parseSseChunk(`event: ${kind}\ndata: ${(msg as MessageEvent).data}\n`);
      if (ev) emitLocal(ev);
    });
  }
}

/** Start the global SSE subscription (idempotent). base defaults to local port 47628. */
export function connectEventStream(base = "http://127.0.0.1:47628"): void {
  if (stream || retryTimer) return;
  open(`${base}/events`);
}

export function disconnectEventStream(): void {
  if (retryTimer) {
    clearTimeout(retryTimer);
    retryTimer = null;
  }
  if (stream) {
    stream.close();
    stream = null;
  }
}

export function lastSeenEventId(): string {
  return lastEventId;
}