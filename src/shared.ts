// OpenCapX/src/shared.ts
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type AgentState = "working" | "waiting" | "done" | "idle";

export interface Choice {
  id: string;
  label: string;
}

export interface AgentEvent {
  id: string;
  agent: string;
  project: string;
  /** Full working directory (nullable). Grouping key: distinguishes ~/a/OpenCapX from ~/b/OpenCapX. */
  cwd?: string;
  /** Sort key from Core (policy lives in Rust); local bubbles without one (e.g. say) sort last. */
  order?: string;
  message: string;
  /** Current model name (hook payload or transcript tail). May be empty. */
  model?: string;
  /** Agent body (last assistant text in the transcript tail), the bubble's second line. */
  speech?: string;
  state: AgentState;
  /** First-seen time of the session (seconds). May be absent in old data. */
  startedAt?: number;
  updatedAt: number;
  choices?: Choice[];
  answered?: string;
}

export function onAgentEvent(cb: (e: AgentEvent) => void): Promise<UnlistenFn> {
  return listen<{ type?: string; kind?: string; payload: AgentEvent }>("opencapx-event", (ev) => {
    const e = ev.payload;
    const kind = e.type ?? e.kind ?? "";
    if (kind.startsWith("agent.")) cb(e.payload);
  });
}

export function mergeSession(list: AgentEvent[], e: AgentEvent): AgentEvent[] {
  const i = list.findIndex((s) => s.id === e.id);
  if (i >= 0) {
    const next = list.slice();
    next[i] = e;
    return next;
  }
  return [...list, e];
}

/** Send the user's choice back: POST /event {id, choices, answered} makes core re-emit the same session with answered=xxx. */
export async function answerChoice(
  id: string,
  agent: string,
  choices: Choice[],
  picked: string,
): Promise<void> {
  try {
    await fetch("http://127.0.0.1:47628/event", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        id,
        agent,
        choices,
        answered: picked,
      }),
    });
  } catch {
    /* listener unavailable */
  }
}
