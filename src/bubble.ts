import { answerChoice, type AgentEvent, type AgentState, type Choice } from "./shared";
import { t, type I18nKey } from "./i18n";

export type BubbleMode = "list" | "carousel" | "compact" | "focus";
export type BubblePos = "right" | "left" | "top" | "bottom";
export type BubbleTheme =
  | "chef"
  | "engineer"
  | "wizard"
  | "explorer"
  | "scientist"
  | "minimal"
  | "paper"
  | "cyber"
  | "terminal"
  | "pixel"
  | "manga"
  | "blueprint";

/** Single source of truth for the theme list: the settings options and overlay validation both read it. */
export const BUBBLE_THEMES: readonly BubbleTheme[] = [
  "chef",
  "engineer",
  "wizard",
  "explorer",
  "scientist",
  "minimal",
  "paper",
  "cyber",
  "terminal",
  "pixel",
  "manga",
  "blueprint",
];

/** Which side of the pet the bubble can sit on. */
export const BUBBLE_POSITIONS: readonly BubblePos[] = ["right", "left", "top", "bottom"];

/** Bubble information density: tight has only the main line; standard adds the body; rich adds model/elapsed/path. */
export type BubbleDensity = "tight" | "standard" | "rich";
export const BUBBLE_DENSITIES: readonly BubbleDensity[] = ["tight", "standard", "rich"];


export interface BubbleOpts {
  mode: BubbleMode;
  theme: BubbleTheme;
  maxRows: number;
  /** Information density, defaults to standard. */
  density?: BubbleDensity;
  /** cwd → meta info (branch/short path). When absent, the group header shows only the project name. */
  projects?: ReadonlyMap<string, ProjectMeta>;
  /** Expanded rows (session id). The bubble re-renders every second, so expansion state must be held externally. */
  expandedIds?: ReadonlySet<string>;
  customMessages?: Partial<Record<string, Partial<Record<AgentState, string>>>>;
}

// Persona copy per theme: i18n keys, not literals — the fun of a persona theme only exists for the
// user if the phrasing lands in their language. Every theme must define all four states.
const PHRASE_KEYS: Record<BubbleTheme, Record<AgentState, I18nKey>> = {
  chef: {
    working: "phraseChefWorking",
    waiting: "phraseChefWaiting",
    done: "phraseChefDone",
    idle: "phraseChefIdle",
  },
  engineer: {
    working: "phraseEngineerWorking",
    waiting: "phraseEngineerWaiting",
    done: "phraseEngineerDone",
    idle: "phraseEngineerIdle",
  },
  wizard: {
    working: "phraseWizardWorking",
    waiting: "phraseWizardWaiting",
    done: "phraseWizardDone",
    idle: "phraseWizardIdle",
  },
  explorer: {
    working: "phraseExplorerWorking",
    waiting: "phraseExplorerWaiting",
    done: "phraseExplorerDone",
    idle: "phraseExplorerIdle",
  },
  scientist: {
    working: "phraseScientistWorking",
    waiting: "phraseScientistWaiting",
    done: "phraseScientistDone",
    idle: "phraseScientistIdle",
  },
  minimal: {
    working: "phraseMinimalWorking",
    waiting: "phraseMinimalWaiting",
    done: "phraseMinimalDone",
    idle: "phraseMinimalIdle",
  },
  paper: {
    working: "phrasePaperWorking",
    waiting: "phrasePaperWaiting",
    done: "phrasePaperDone",
    idle: "phrasePaperIdle",
  },
  cyber: {
    working: "phraseCyberWorking",
    waiting: "phraseCyberWaiting",
    done: "phraseCyberDone",
    idle: "phraseCyberIdle",
  },
  terminal: {
    working: "phraseTerminalWorking",
    waiting: "phraseTerminalWaiting",
    done: "phraseTerminalDone",
    idle: "phraseTerminalIdle",
  },
  pixel: {
    working: "phrasePixelWorking",
    waiting: "phrasePixelWaiting",
    done: "phrasePixelDone",
    idle: "phrasePixelIdle",
  },
  manga: {
    working: "phraseMangaWorking",
    waiting: "phraseMangaWaiting",
    done: "phraseMangaDone",
    idle: "phraseMangaIdle",
  },
  blueprint: {
    working: "phraseBlueprintWorking",
    waiting: "phraseBlueprintWaiting",
    done: "phraseBlueprintDone",
    idle: "phraseBlueprintIdle",
  },
};

export function phraseFor(theme: BubbleTheme, state: AgentState): string {
  return t(PHRASE_KEYS[theme][state]);
}

export function elapsedText(updatedAt: number, now: number): string {
  const s = Math.max(0, Math.floor(now / 1000 - updatedAt));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

/** {count} placeholder substitution (same convention as settings.ts::tlFmt; i18n's t() does no substitution). */
function fillCount(tpl: string, n: number): string {
  return tpl.replace("{count}", String(n));
}

/** Sort key comparison: the policy lives in Rust (core::agent::order_key); here we only compare strings.
    Entries without a key (local say bubbles) sort last. */
function byOrderKey(a: AgentEvent, b: AgentEvent): number {
  const ak = a.order ?? "\uffff";
  const bk = b.order ?? "\uffff";
  return ak < bk ? -1 : ak > bk ? 1 : 0;
}

/** Grouping key: prefer the full cwd (distinguishes same-named projects), fall back to project when cwd is missing. */
function groupKey(s: AgentEvent): string {
  const cwd = (s.cwd ?? "").trim();
  return cwd !== "" ? cwd : s.project;
}

function groupByProject(sessions: AgentEvent[]): Map<string, AgentEvent[]> {
  const g = new Map<string, AgentEvent[]>();
  for (const s of sessions) {
    const key = groupKey(s);
    const list = g.get(key);
    if (list) list.push(s);
    else g.set(key, [s]);
  }
  return g;
}

/** Project meta read from Core (the project_meta command from Task 4). */
export interface ProjectMeta {
  cwd: string;
  short: string;
  branch?: string | null;
}

/** Group header: project · branch · N waiting on you · M working (zero values omitted, same rule as the tray).
    meta supplies branch and short path; when unavailable, show only the project name (branch is enrichment, not a dependency). */
function headerHtml(
  list: AgentEvent[],
  nameCounts: Map<string, number>,
  projects?: ReadonlyMap<string, ProjectMeta>,
): string {
  const key = groupKey(list[0]);
  const meta = projects?.get(key);
  const project = list[0].project || "unknown";
  // Same-named projects (~/a/OpenCapX and ~/b/OpenCapX) have different grouping keys but the same display name → distinguish by short path
  const dup = (nameCounts.get(project) ?? 0) > 1;
  const name = dup && meta?.short ? meta.short : project;
  const stats: string[] = [];
  if (meta?.branch) stats.push(meta.branch);
  const waiting = list.filter((s) => s.state === "waiting").length;
  const working = list.filter((s) => s.state === "working").length;
  if (waiting > 0) stats.push(fillCount(t("groupWaitingCount"), waiting));
  if (working > 0) stats.push(fillCount(t("groupWorkingCount"), working));
  const title = meta?.short ? `${project} — ${meta.short}` : project;
  return `<div class="group-header" title="${esc(title)}">${esc(name)}${stats
    .map((s) => `<span class="group-stat">· ${esc(s)}</span>`)
    .join("")}</div>`;
}

function rowHtml(
  s: AgentEvent,
  theme: BubbleTheme,
  now: number,
  density: BubbleDensity,
  meta: ProjectMeta | undefined,
  expanded: boolean,
): string {
  const custom = "";
  const isDone = s.state === "done";
  // Wow 4: on completion, show a 🎉 + i18n text instead of a silent dot line. For agent users
  // just switching back to the desktop / looking up at the pet, this is the first feedback seen.
  const msg = isDone
    ? `${t("celebrateEmoji")} ${t("celebrateDone")}`
    : s.message || phraseFor(theme, s.state) || custom;
  const cls = `${s.state === "waiting" ? "row waiting" : `row ${s.state}`}${expanded ? " expanded" : ""}`;
  const answeredBadge = s.answered
    ? ` <span class="answer-badge" title="${esc(s.answered)}">✓ ${esc(s.answered)}</span>`
    : "";
  const doneSub = isDone ? `<span class="done-sub">${t("celebrateSub" as I18nKey)}</span>` : "";
  // Second line: what the agent itself said (transcript tail). The done row's msg is replaced by the celebration text,
  // so only this line preserves "what it said last"; on non-done rows it is hidden when it duplicates msg.
  // Density: tight hides the body.
  const speech =
    density === "tight"
      ? ""
      : ((isDone ? s.speech : s.speech === msg ? "" : s.speech) ?? "").trim();
  const speechHtml = speech ? `<span class="speech" title="${esc(speech)}">${esc(speech)}</span>` : "";
  const choiceHtml =
    s.choices && s.choices.length > 0 && !s.answered
      ? `<div class="choices">${s.choices
          .map(
            (c) =>
              `<button class="choice-btn" data-pick="${esc(c.id)}" data-sid="${esc(s.id)}" data-agent="${esc(s.agent)}" data-choices="${esc(JSON.stringify(s.choices))}" type="button">${esc(c.label)}</button>`,
          )
          .join("")}</div>`
      : "";
  // Model badge, elapsed time, and short path show only in rich (filled from the hook payload or transcript tail)
  const model = density === "rich" ? (s.model ?? "").trim() : "";
  const modelHtml = model ? `<span class="model" title="${esc(model)}">${esc(model)}</span>` : "";
  // Timing semantics: in-progress sessions show "how long the session has run" (startedAt); finished ones show
  // "how long since it finished" (updatedAt). Using updatedAt alone would make long continuously-working sessions always show 2s.
  const live = s.state === "working" || s.state === "waiting";
  const since = live ? (s.startedAt ?? s.updatedAt) : s.updatedAt;
  const elapsedHtml =
    density === "rich" ? `<span class="elapsed">${elapsedText(since, now)}</span>` : "";
  const pathHtml =
    density === "rich" && meta?.short
      ? `<span class="path" title="${esc(meta.short)}">${esc(meta.short)}</span>`
      : "";
  // The first line is "who · doing what"; the body and choices each take their own line (no flex-wrap: that would push
  // long messages, along with model/timing, onto a second line, and the main line would no longer be a single line).
  const line = `<span class="line"><span class="dot"></span><span class="agent" title="${esc(s.agent)}">${s.agent}</span><span class="sep">·</span><span class="msg" title="${esc(msg)}">${msg}${answeredBadge}</span>${doneSub}${modelHtml}${elapsedHtml}${pathHtml}</span>`;
  return `<div class="${cls}" data-row-sid="${esc(s.id)}">${line}${speechHtml}${choiceHtml}</div>`;
}

function esc(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

export function renderBubble(
  el: HTMLElement,
  sessions: AgentEvent[],
  opts: BubbleOpts,
): void {
  el.dataset.mode = opts.mode;
  // Theme attribute: bubble-themes.css uses it to provide colors + shapes (the settings preview block uses the same attribute)
  el.dataset.bubbleTheme = opts.theme;
  const now = Date.now();
  if (opts.mode === "compact") {
    const working = sessions.filter((s) => s.state === "working").length;
    const waiting = sessions.filter((s) => s.state === "waiting").length;
    el.innerHTML = `<div class="compact">${working} working · ${waiting} waiting</div>`;
    return;
  }
  // Locally merged events must land back in their ordered position, so we re-sort by Core's key here.
  // Slicing happens before grouping: when a whole group is cut, no empty group header is produced.
  const ordered = [...sessions].sort(byOrderKey);
  const rows = ordered.slice(0, opts.maxRows);
  // Rows cut by maxRows no longer vanish silently: total minus visible tells how many remain.
  const hidden = ordered.length - rows.length;
  if (opts.mode === "carousel" && rows.length > 1) {
    const idx = Math.floor(now / 3000) % rows.length;
    const dots = rows.map((_, i) => `<span class="dot-nav${i === idx ? " on" : ""}"></span>`).join("");
    const row = rows[idx];
    const open = opts.expandedIds?.has(row.id) ?? false;
    el.innerHTML = `${rowHtml(row, opts.theme, now, opts.density ?? "standard", opts.projects?.get(groupKey(row)), open)}<div class="dots">${dots}</div>`;
    bindChoiceHandlers(el);
    return;
  }
  // Focus: exactly one row — the first in Core's priority order (a waiting session outranks a working one,
  // the newest working one outranks older ones). Everything else is hidden, but counted below the row,
  // the same "don't vanish silently" rule as the maxRows cut. It differs from carousel: carousel rotates
  // through everyone, focus only ever promotes the single row that needs you.
  if (opts.mode === "focus" && rows.length > 0) {
    const row = rows[0];
    const open = opts.expandedIds?.has(row.id) ?? false;
    const rest = ordered.length - 1;
    const restHtml = rest > 0 ? `<div class="focus-rest">${esc(t("focusRest").replace("{n}", String(rest)))}</div>` : "";
    el.innerHTML = `${rowHtml(row, opts.theme, now, opts.density ?? "standard", opts.projects?.get(groupKey(row)), open)}${restHtml}`;
    bindChoiceHandlers(el);
    return;
  }
  // rows are already sorted by Core's order key: a group's "first appearance order" is its priority order,
  // and within a group the same order holds — no need (and no reason) to sort again on the frontend.
  const groups = groupByProject(rows);
  const nameCounts = new Map<string, number>();
  for (const list of groups.values()) {
    const p = list[0].project || "unknown";
    nameCounts.set(p, (nameCounts.get(p) ?? 0) + 1);
  }
  const html = [...groups.entries()]
    .map(
      ([, list]) =>
        `<div class="group" data-project="${esc(list[0].project)}">${headerHtml(list, nameCounts, opts.projects)}${list
          .map((s) =>
            rowHtml(
              s,
              opts.theme,
              now,
              opts.density ?? "standard",
              opts.projects?.get(groupKey(s)),
              opts.expandedIds?.has(s.id) ?? false,
            ),
          )
          .join("")}</div>`,
    )
    .join("");
  el.innerHTML =
    html + (hidden > 0 ? `<div class="more">${esc(fillCount(t("moreSessions"), hidden))}</div>` : "");
  bindChoiceHandlers(el);
}

/** Attach one-shot click handlers to every choice-btn in the bubble. Re-rendering the bubble re-attaches them. */
function bindChoiceHandlers(el: HTMLElement): void {
  el.querySelectorAll<HTMLButtonElement>("button.choice-btn").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const sid = btn.dataset.sid ?? "";
      const agent = btn.dataset.agent ?? "";
      const pick = btn.dataset.pick ?? "";
      let choices: Choice[] = [];
      try {
        choices = JSON.parse(btn.dataset.choices ?? "[]") as Choice[];
      } catch {
        choices = [];
      }
      btn.disabled = true;
      btn.parentElement?.querySelectorAll<HTMLButtonElement>("button.choice-btn").forEach((b) => {
        b.disabled = true;
      });
      await answerChoice(sid, agent, choices, pick);
    });
  });
}
