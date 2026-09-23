import { invoke } from "@tauri-apps/api/core";
import { currentMonitor, getCurrentWindow } from "@tauri-apps/api/window";
import { LogicalPosition, LogicalSize } from "@tauri-apps/api/dpi";
import { listen } from "@tauri-apps/api/event";
import { mergeSession, onAgentEvent, type AgentEvent, type AgentState } from "./shared";
import {
  BUBBLE_DENSITIES,
  BUBBLE_POSITIONS,
  BUBBLE_THEMES,
  renderBubble,
  type BubbleDensity,
  type BubbleMode,
  type BubblePos,
  type BubbleTheme,
  type ProjectMeta,
} from "./bubble";
import { connectEventStream, onEvent } from "./events";
import { soundForTransition } from "./sounds";
import { MOOD_ROWS } from "./sprite";
import {
  loadModelPack,
  loadPack,
  rowForMood,
  sliceSheet,
  type PackFrame,
  type SlicedPack,
} from "./petpack";
import { Pet3D, type Mood3D } from "./pet3d";
import { setLocale, t, type I18nKey, type Locale } from "./i18n";
import { requestPermission, sendNotification } from "@tauri-apps/plugin-notification";

const FPS: Record<AgentState, number> = { working: 8, waiting: 4, done: 4, idle: 3 };
const MOOD_COLORS: Record<AgentState, string> = {
  working: "#4ade80",
  waiting: "#fb923c",
  done: "#60a5fa",
  idle: "#888888",
};
const BASE_PX = 120;

// Size budget for the pet window (logical pixels).
// PET_WINDOW_W must simultaneously equal the pet window's width in tauri.conf.json
// and the window width used when computing bubble width from 100vw in overlay.css;
// BUBBLE_BUDGET corresponds to the bubble's 320px max-width/max-height cap.
const PET_WINDOW_W = 440;
const BUBBLE_BUDGET = 320;

/** Where the pet "should" sit in screen coordinates; null = not recorded yet (or the user just dragged it). */
let petAnchor: { x: number; y: number } | null = null;

/** Logical window height last requested by us; null = not applied yet. Lets the petSize sync skip no-op resizes. */
let appliedWindowH: number | null = null;

// pet.state text: i18n key + state → CSS class. Aligned with the Agent State Protocol
// (i2 §12, mcp.md): idle|thinking|working|waiting|permission|success|error|sleeping.
type PetState = "idle" | "thinking" | "working" | "waiting" | "permission" | "success" | "error" | "sleeping";
const PET_STATE_I18N: Record<PetState, I18nKey> = {
  idle: "petStateIdle",
  thinking: "petStateThinking",
  working: "petStateWorking",
  waiting: "petStateWaiting",
  permission: "petStatePermission",
  success: "petStateSuccess",
  error: "petStateError",
  sleeping: "petStateSleeping",
};
const PET_STATE_CLASS: Record<PetState, string> = {
  idle: "state-idle",
  thinking: "state-thinking",
  working: "state-working",
  waiting: "state-waiting",
  permission: "state-permission",
  success: "state-success",
  error: "state-error",
  sleeping: "state-sleeping",
};
const VALID_PET_STATES = new Set<PetState>([
  "idle",
  "thinking",
  "working",
  "waiting",
  "permission",
  "success",
  "error",
  "sleeping",
]);

interface OverlayPrefs {
  mode: BubbleMode;
  theme: BubbleTheme;
  /** Which side of the pet the bubble is on (default right). */
  bubblePos: BubblePos;
  /** Space between the pet frame and the bubble, logical px 0..24. */
  bubbleGap: number;
  maxRows: number;
  /** Information density (tight / standard / rich). */
  density: BubbleDensity;
  petSize: number;
  petSheet: string;
  petPack: string;
  bubbleEnabled: boolean;
  bubbleDuration: number;
  petVisible: boolean;
  breakEnabled: boolean;
  breakMinutes: number;
}

const DEFAULT_PREFS: OverlayPrefs = { mode: "carousel", theme: "chef", bubblePos: "right", bubbleGap: 0, maxRows: 5, density: "standard", petSize: 100, petSheet: "", petPack: "", bubbleEnabled: true, bubbleDuration: 5, petVisible: true, breakEnabled: false, breakMinutes: 60 };

const MODES: readonly string[] = ["list", "carousel", "compact", "focus"];

/** A user-supplied image (URL or upload): always sliced by alpha gaps; a single image is 1 frame. */
let petSheetImg: HTMLImageElement | null = null;
let petSheetFrames: PackFrame[][] | null = null;
let petSrc = "";
// Selected pet pack (~/.opencapx/pets/<id>). Takes priority over petSheet and the built-in logo:
// packs are auto-sliced by alpha, whereas petSheet is a hardcoded 8×9 grid.
let petPackRows: SlicedPack | null = null;
let packSrc = "";
// 3D pack (pet.json kind=3d): WebGL rendering, a completely different branch from 2D;
// three is dynamically imported only when a 3D pack is actually selected.
let pet3d: Pet3D | null = null;

// Built-in pet figure: the tool logo (drawn whole, no sprite-sheet slicing). When the user sets petSheet, a custom sprite sheet is used instead.
const PET_LOGO_SRC = "/pet-logo.png";
let petLogo: HTMLImageElement | null = null;
{
  const img = new Image();
  img.src = PET_LOGO_SRC;
  img.onload = () => {
    petLogo = img;
  };
}

let prefs: OverlayPrefs = { ...DEFAULT_PREFS };
let sessions: AgentEvent[] = [];
let lastActivity = 0;
let activeSince = 0;
// Expansion state of bubble rows. Must live outside the DOM: the bubble re-renders every second and DOM-attached state gets wiped.
const expandedRows = new Set<string>();

// Manual MCP set_state override, falling back to the aggregated agent state after 15s
const PET_STATE_MAP: Record<string, AgentState> = {
  idle: "idle",
  thinking: "working",
  working: "working",
  waiting: "waiting",
  permission: "waiting",
  success: "done",
  error: "waiting",
  sleeping: "idle",
};
let manualMood: { state: AgentState; until: number } | null = null;

let askBox: HTMLDivElement | null = null;

function clearAsk(): void {
  if (askBox) {
    askBox.remove();
    askBox = null;
  }
}

// opencapx.ask form fields (mcp.md v1.1); fields pass through verbatim after Core validation.
interface AskField {
  name: string;
  label?: string;
  type: "text" | "number" | "select" | "checkbox";
  placeholder?: string;
  default?: unknown;
  options?: string[];
  required?: boolean;
}

const ASK_BTN_STYLE =
  "padding:4px 10px;border-radius:8px;border:1px solid rgba(255,255,255,0.25);background:#3b3b45;color:#fff;font-size:12px;cursor:pointer;";
const ASK_INPUT_STYLE =
  "padding:4px 8px;border-radius:8px;border:1px solid rgba(255,255,255,0.25);background:#2a2a32;color:#fff;font-size:12px;width:100%;box-sizing:border-box;";

function markMissing(el: HTMLElement): void {
  el.style.borderColor = "#f87171";
}

function renderAsk(
  bubbleRoot: HTMLElement | null,
  payload: {
    id: string;
    question: string;
    options?: { label: string; value: string }[];
    multi?: boolean;
    fields?: AskField[];
    command?: string;
  },
): void {
  clearAsk();
  if (!bubbleRoot) return;
  const command = payload.command ?? "answer_ask";
  const box = document.createElement("div");
  box.style.cssText =
    "margin-top:8px;padding:8px 10px;background:rgba(20,20,24,0.92);border:1px solid rgba(255,255,255,0.18);border-radius:10px;color:#eee;font-size:12px;display:flex;flex-direction:column;gap:6px;";
  const q = document.createElement("div");
  q.textContent = payload.question;
  // overflow-wrap: a long question (or a pasted path/URL) must wrap, not push out the bubble's fixed width
  q.style.cssText = "font-weight:600;overflow-wrap:anywhere;";
  box.appendChild(q);

  // Form type (fields): Core has already validated the shape; here we render + check required
  if (payload.fields && payload.fields.length > 0) {
    const inputs = new Map<string, HTMLInputElement | HTMLSelectElement>();
    for (const f of payload.fields) {
      const wrap = document.createElement("div");
      wrap.style.cssText = "display:flex;flex-direction:column;gap:2px;";
      if (f.label) {
        const lab = document.createElement("div");
        lab.textContent = f.required ? `${f.label} *` : f.label;
        lab.style.cssText = "opacity:0.85;font-size:11px;";
        wrap.appendChild(lab);
      }
      if (f.type === "select") {
        const sel = document.createElement("select");
        sel.style.cssText = ASK_INPUT_STYLE;
        for (const o of f.options ?? []) {
          const optEl = document.createElement("option");
          optEl.textContent = o;
          optEl.value = o;
          sel.appendChild(optEl);
        }
        if (typeof f.default === "string") sel.value = f.default;
        inputs.set(f.name, sel);
        wrap.appendChild(sel);
      } else {
        const inp = document.createElement("input");
        inp.type = f.type === "checkbox" ? "checkbox" : f.type;
        if (f.placeholder) inp.placeholder = f.placeholder;
        if (f.type === "checkbox") {
          inp.checked = f.default === true;
          inp.style.cssText = "width:auto;";
        } else {
          inp.style.cssText = ASK_INPUT_STYLE;
          if (f.default !== undefined) inp.value = String(f.default);
        }
        inp.addEventListener("input", () => {
          inp.style.borderColor = "rgba(255,255,255,0.25)";
        });
        inputs.set(f.name, inp);
        wrap.appendChild(inp);
      }
      box.appendChild(wrap);
    }
    const submit = document.createElement("button");
    submit.textContent = t("askSubmit");
    submit.style.cssText = ASK_BTN_STYLE + "background:#3b5bdb;";
    submit.addEventListener("click", () => {
      const answer: Record<string, unknown> = {};
      for (const f of payload.fields ?? []) {
        const el = inputs.get(f.name);
        if (!el) continue;
        if (el instanceof HTMLInputElement && f.type === "checkbox") {
          answer[f.name] = el.checked;
          continue;
        }
        const raw = el.value.trim();
        if (raw === "") {
          if (f.required) {
            markMissing(el);
            return;
          }
          answer[f.name] = f.default !== undefined ? f.default : "";
          continue;
        }
        if (f.type === "number") {
          const n = Number(raw);
          answer[f.name] = Number.isFinite(n) ? n : raw;
        } else {
          answer[f.name] = raw;
        }
      }
      void invoke(command, { id: payload.id, answer: JSON.stringify(answer) }).catch((err) =>
        console.warn(`ask: failed to deliver answer via ${command}`, err),
      );
      clearAsk();
    });
    box.appendChild(submit);
    bubbleRoot.appendChild(box);
    askBox = box;
    return;
  }

  const opts = payload.options ?? [];
  const row = document.createElement("div");
  row.style.cssText = "display:flex;gap:6px;flex-wrap:wrap;";

  // Multi-select (options + multi): toggle + submit, answer is a JSON array string
  if (payload.multi) {
    const selected = new Set<string>();
    for (const opt of opts) {
      const btn = document.createElement("button");
      btn.textContent = opt.label;
      btn.style.cssText = ASK_BTN_STYLE;
      btn.addEventListener("click", () => {
        if (selected.has(opt.value)) {
          selected.delete(opt.value);
          btn.style.background = "#3b3b45";
          btn.style.borderColor = "rgba(255,255,255,0.25)";
        } else {
          selected.add(opt.value);
          btn.style.background = "#3b5bdb";
          btn.style.borderColor = "#91a7ff";
        }
        row.style.borderColor = "rgba(255,255,255,0.18)";
      });
      row.appendChild(btn);
    }
    box.appendChild(row);
    const submit = document.createElement("button");
    submit.textContent = t("askSubmit");
    submit.style.cssText = ASK_BTN_STYLE + "background:#3b5bdb;";
    submit.addEventListener("click", () => {
      if (selected.size === 0) {
        row.style.borderColor = "#f87171";
        setTimeout(() => (row.style.borderColor = "rgba(255,255,255,0.18)"), 800);
        return;
      }
      const answer = JSON.stringify([...selected]);
      void invoke(command, { id: payload.id, answer }).catch((err) =>
        console.warn(`ask: failed to deliver answer via ${command}`, err),
      );
      clearAsk();
    });
    box.appendChild(submit);
    bubbleRoot.appendChild(box);
    askBox = box;
    return;
  }

  // Single-select (the v1 original path, including permission dialogs): click to answer, answer is plain text
  for (const opt of opts) {
    const btn = document.createElement("button");
    btn.textContent = opt.label;
    btn.style.cssText = ASK_BTN_STYLE;
    btn.addEventListener("click", () => {
      void invoke(command, { id: payload.id, answer: opt.value }).catch((err) =>
        console.warn(`ask: failed to deliver answer via ${command}`, err),
      );
      clearAsk();
    });
    row.appendChild(btn);
  }
  box.appendChild(row);
  bubbleRoot.appendChild(box);
  askBox = box;
}

async function loadPrefs(): Promise<void> {
  try {
    const s = await invoke<Record<string, unknown>>("get_settings");
    const mode = String(s.mode ?? "carousel");
    const theme = String(s.bubbleTheme ?? "chef");
    const pos = String(s.bubblePos ?? "right");
    const bubbleGap = Math.min(24, Math.max(0, Number(s.bubbleGap ?? 0) || 0));
    const maxRows = Math.min(10, Math.max(1, Number(s.maxRows ?? 5) || 5));
    const density = String(s.bubbleDensity ?? "standard");
    const petSize = Math.min(130, Math.max(70, Number(s.petSize ?? 100) || 100));
    const petSheet = String(s.petSheet ?? "");
    const petPack = String(s.petPack ?? "");
    const bubbleEnabled = s.bubbleEnabled !== false;
    const bubbleDuration = Math.min(300, Math.max(0, Number(s.bubbleDuration ?? 5) || 0));
    const petVisible = s.petVisible !== false;
    const breakEnabled = s.breakEnabled === true;
    const breakMinutes = Math.min(480, Math.max(5, Number(s.breakMinutes ?? 60) || 60));
    const loc = String(s.locale ?? "en");
    if (loc === "en" || loc === "vi" || loc === "zh-Hans") setLocale(loc as Locale);
    prefs = {
      mode: MODES.includes(mode) ? (mode as BubbleMode) : "carousel",
      theme: (BUBBLE_THEMES as readonly string[]).includes(theme)
        ? (theme as BubbleTheme)
        : "chef",
      bubblePos: (BUBBLE_POSITIONS as readonly string[]).includes(pos)
        ? (pos as BubblePos)
        : "right",
      bubbleGap,
      maxRows,
      density: (BUBBLE_DENSITIES as readonly string[]).includes(density)
        ? (density as BubbleDensity)
        : "standard",
      petSize,
      petSheet,
      petPack,
      bubbleEnabled,
      bubbleDuration,
      petVisible,
      breakEnabled,
      breakMinutes,
    };
    if (petSheet !== petSrc) {
      petSrc = petSheet;
      petSheetImg = null;
      petSheetFrames = null;
      if (petSheet) {
        const img = new Image();
        img.src = petSheet;
        img.onload = () => {
          if (petSrc !== petSheet) return;
          petSheetImg = img;
          // Always slice by alpha: a single image → 1 frame (drawn whole), a sheet → auto-detected rows/columns.
          // When pixels cannot be read (cross-origin), degrade to "one whole-image frame" rather than drawing nothing.
          const rows = sliceSheet(img);
          petSheetFrames =
            rows.length > 0
              ? rows
              : [[{ x: 0, y: 0, w: img.naturalWidth, h: img.naturalHeight }]];
        };
      }
    }
    if (petPack !== packSrc) {
      packSrc = petPack;
      petPackRows = null;
      if (petPack) {
        void loadPack(petPack).then((p) => {
          if (packSrc === petPack && p) petPackRows = p;
        });
        void setupPet3d(petPack);
      } else {
        teardownPet3d();
      }
    }
  } catch {
    prefs = { ...DEFAULT_PREFS };
  }
}

/** Window height for a layout: "bottom" stacks pet + gap + bubble vertically; every other side fits in the bubble budget. */
function windowHeightFor(pos: BubblePos, petPx: number, gap: number): number {
  return pos === "bottom" ? petPx + gap + BUBBLE_BUDGET : BUBBLE_BUDGET;
}

/** Give the compositor frames to apply the new viewport size: the pet rect in the top/bottom layouts is
 *  pinned to 100vh, and reading it before the resize lands computes the anchor from the old height. */
function settleViewport(): Promise<void> {
  return new Promise((resolve) =>
    requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
  );
}

/** petSize/gap changed under the current layout: the window height must follow, or the bubble is clipped when the
 *  pet grows and dead space is left when it shrinks. The pet is pinned top:0 and setSize keeps the window
 *  origin, so no anchor dance is needed here. */
async function syncWindowHeight(): Promise<void> {
  // Same effective side as applyBubblePos: the edge auto-flip may have mirrored the user's side,
  // and the height must follow the layout that is actually on screen — otherwise the sync would
  // shrink a flipped-to-bottom window right back down every tick.
  const pos =
    flipCache && flipCache.want === prefs.bubblePos ? flipCache.eff : prefs.bubblePos;
  const petPx = Math.round((BASE_PX * prefs.petSize) / 100);
  const wantH = windowHeightFor(pos, petPx, prefs.bubbleGap);
  if (wantH === appliedWindowH) return;
  try {
    await getCurrentWindow().setSize(new LogicalSize(PET_WINDOW_W, wantH));
    appliedWindowH = wantH;
  } catch (err) {
    console.warn("syncWindowHeight: failed to resize the window, bubble space may be insufficient", err);
  }
}

/** Logical bounds of the monitor the window is on; null when the platform refuses. */
async function monitorBounds(): Promise<{ x: number; y: number; w: number; h: number } | null> {
  try {
    const m = await currentMonitor();
    if (!m) return null;
    return {
      x: m.position.x / m.scaleFactor,
      y: m.position.y / m.scaleFactor,
      w: m.size.width / m.scaleFactor,
      h: m.size.height / m.scaleFactor,
    };
  } catch {
    return null;
  }
}

/**
 * Move the bubble to the pet's other side.
 *
 * The pet frame would otherwise be pinned to the window's other corner, making the pet "jump" on screen — so when switching sides
 * we record the pet's position in screen coordinates, then move the window back after layout to keep it in place.
 *
 * Top/bottom layouts also need the window made taller (pet frame + bubble budget): moving the window alone is not enough,
 * because when the window hits the top screen edge the system clamps its position and the pet gets shoved along.
 *
 * Edge auto-flip: when the pet sits so close to a screen edge that the bubble's full budget would cross it, the side
 * mirrors (right↔left, top↔bottom) and the pet stays put — only the bubble moves to where the room actually is.
 * The decision is cached per (user side, pet anchor, pet size, gap) so the per-second poll costs nothing when nothing changed.
 */
let flipCache: { want: BubblePos; ax: number; ay: number; petPx: number; gap: number; eff: BubblePos } | null = null;

async function applyBubblePos(): Promise<void> {
  const want = prefs.bubblePos;
  const petPx = Math.round((BASE_PX * prefs.petSize) / 100);
  const gap = prefs.bubbleGap;
  // Fast path: same user side, same recorded anchor and metrics, layout already applied → nothing can have changed.
  if (
    document.body.dataset.bpos &&
    flipCache &&
    petAnchor &&
    flipCache.want === want &&
    flipCache.ax === petAnchor.x &&
    flipCache.ay === petAnchor.y &&
    flipCache.petPx === petPx &&
    flipCache.gap === gap
  ) {
    return;
  }
  const petEl = (pet3d
    ? document.getElementById("pet3d")
    : document.getElementById("pet")) as HTMLElement | null;
  if (!petEl) return;
  const win = getCurrentWindow();
  const before = petEl.getBoundingClientRect();
  // Target = the screen position where the pet "should" be. Use the anchor when present, not the previous actual position —
  // otherwise, after the window is clamped at a screen edge, the error is inherited by the next side switch ("top" gets pushed down, "bottom"
  // keeps computing from that misalignment). When the user has dragged the pet, the anchor is stale, so only then re-record from the current position.
  let anchor: { x: number; y: number } | null = null;
  try {
    const scale = await win.scaleFactor();
    const at = await win.outerPosition();
    anchor = petAnchor ?? { x: at.x / scale + before.left, y: at.y / scale + before.top };
  } catch (err) {
    console.warn("applyBubblePos: cannot read window position, the pet will follow the side switch", err);
  }
  // Effective side: the user's choice, mirrored when the full bubble budget would cross the monitor edge.
  // Full budget (not "whatever fits") keeps the rule simple: a half-width bubble at the edge is worse than a flipped one.
  let eff = want;
  if (anchor) {
    const mb = await monitorBounds();
    if (mb) {
      const need = gap + BUBBLE_BUDGET;
      if (want === "right" && anchor.x + petPx + need > mb.x + mb.w) eff = "left";
      else if (want === "left" && anchor.x - need < mb.x) eff = "right";
      else if (want === "bottom" && anchor.y + petPx + need > mb.y + mb.h) eff = "top";
      else if (want === "top" && anchor.y - need < mb.y) eff = "bottom";
    }
    flipCache = { want, ax: anchor.x, ay: anchor.y, petPx, gap, eff };
  }
  if (document.body.dataset.bpos === eff) {
    // Layout already correct (e.g. the HTML default "right" on first run, or a flip that happens
    // to match the newly picked side): adopt the anchor so the per-second fast path can engage.
    if (anchor) petAnchor = anchor;
    return;
  }
  document.body.dataset.bpos = eff;
  // Only grow the window for "bottom" layout: the window grows downward from the top-left corner, needs no space above the pet,
  // and the bubble gets the full size budget. Conversely, a "top" layout growing upward needs
  // more headroom and more easily hits the screen edge and gets pushed back by the system, shoving the pet away.
  try {
    const wantH = windowHeightFor(eff, petPx, prefs.bubbleGap);
    await win.setSize(new LogicalSize(PET_WINDOW_W, wantH));
    appliedWindowH = wantH;
    await settleViewport();
  } catch (err) {
    console.warn("applyBubblePos: failed to resize the window, bubble space may be insufficient", err);
  }
  const after = petEl.getBoundingClientRect(); // reading the rect forces a reflow, so the order cannot change
  if (!anchor) return;
  try {
    await win.setPosition(new LogicalPosition(anchor.x - after.left, anchor.y - after.top));
    petAnchor = anchor;
  } catch (err) {
    // If the window cannot be moved, still switch the layout (the bubble is on the correct side), but a trace must be left:
    // missing permissions hid for a long time behind "silently swallowed"
    console.warn("applyBubblePos: cannot move the window, the pet will follow the side switch", err);
  }
}

function applyPetSize(canvas: HTMLCanvasElement): void {
  const px = Math.round((BASE_PX * prefs.petSize) / 100);
  if (canvas.width !== px) canvas.width = px;
  if (canvas.height !== px) canvas.height = px;
  document.documentElement.style.setProperty("--pet-w", `${px}px`);
  // Pet-to-bubble spacing is applied here too: this runs every poll tick, so a gap change lands within a second.
  document.documentElement.style.setProperty("--bubble-gap", `${prefs.bubbleGap}px`);
  pet3d?.setSize(px); // the 3D renderer must follow the size too
}

/** 3D pack: lazy-load three → load model → start rendering. On failure/side switch, quietly fall back to 2D. */
async function setupPet3d(id: string): Promise<void> {
  const pack = await loadModelPack(id);
  if (packSrc !== id) return; // already switched to another pack
  if (!pack) {
    // 2D pack (or 3D load failure): the 3D branch must be torn down entirely,
    // otherwise is3d keeps the 2D canvas hidden and the pet stays on the previous 3D model.
    teardownPet3d();
    return;
  }
  const canvas3d = document.getElementById("pet3d") as HTMLCanvasElement | null;
  if (!canvas3d) return;
  teardownPet3d();
  try {
    const inst = await Pet3D.create(canvas3d);
    await inst.load(pack.data, pack.meta.clips ?? {});
    if (packSrc !== id) {
      inst.dispose();
      return;
    }
    pet3d = inst;
    document.body.classList.add("is3d");
    pet3d.setSize(Math.round((BASE_PX * prefs.petSize) / 100));
    setPet3dRunning(true);
  } catch (err) {
    console.warn("pet3d: model load failed, falling back to 2D", err);
    teardownPet3d();
  }
}

function teardownPet3d(): void {
  document.body.classList.remove("is3d");
  if (pet3d) {
    pet3d.dispose();
    pet3d = null;
  }
}

/** Run/pause 3D rendering: stop when the pet is hidden or the document is invisible, so a persistent window does not keep burning GPU. */
function setPet3dRunning(on: boolean): void {
  if (!pet3d) return;
  if (on && document.visibilityState !== "hidden") pet3d.start();
  else pet3d.stop();
}

export function aggregate(sessions: AgentEvent[]): AgentState {
  let mood: AgentState = "idle";
  for (const s of sessions) {
    if (s.state === "working") return "working";
    if (s.state === "waiting") mood = "waiting";
    else if (s.state === "done" && mood !== "waiting") mood = "done";
  }
  return mood;
}

/** Draw the built-in logo whole, expressing state through procedural transforms (no sprite-sheet slicing). */
function drawLogo(
  ctx: CanvasRenderingContext2D,
  canvas: HTMLCanvasElement,
  mood: AgentState,
  frame: number,
  celebrating: boolean,
): boolean {
  if (!petLogo || !petLogo.complete || petLogo.naturalWidth === 0) return false;
  drawStill(ctx, canvas, petLogo, null, mood, frame, celebrating);
  return true;
}

/** Draw one frame scaled proportionally and centered. When `f` is null, draw the whole image. */
function drawFrameFit(
  ctx: CanvasRenderingContext2D,
  canvas: HTMLCanvasElement,
  img: HTMLImageElement,
  f: PackFrame | null,
): void {
  const sw = f ? f.w : img.naturalWidth;
  const sh = f ? f.h : img.naturalHeight;
  const scale = Math.min(canvas.width / sw, canvas.height / sh);
  const dw = sw * scale;
  const dh = sh * scale;
  ctx.drawImage(
    img,
    f ? f.x : 0,
    f ? f.y : 0,
    sw,
    sh,
    (canvas.width - dw) / 2,
    (canvas.height - dh) / 2,
    dw,
    dh,
  );
}

/**
 * State animation for static figures (single-frame images / built-in logo): draw whole + procedural transforms.
 * A single PNG should not be sliced as a sheet — that cuts out transparent fragments and looks like "nothing displayed".
 */
function drawStill(
  ctx: CanvasRenderingContext2D,
  canvas: HTMLCanvasElement,
  img: HTMLImageElement,
  f: PackFrame | null,
  mood: AgentState,
  frame: number,
  celebrating: boolean,
): void {
  let scale = 1;
  let dy = 0;
  let rot = 0;
  if (celebrating) {
    dy = -Math.abs(Math.sin(frame / 2)) * canvas.height * 0.08;
    scale = 1 + Math.abs(Math.sin(frame / 2)) * 0.08;
  } else if (mood === "working") {
    rot = Math.sin(frame / 2) * 0.05;
    dy = Math.sin(frame / 2) * canvas.height * 0.012;
  } else if (mood === "waiting") {
    scale = 1 + Math.sin(frame / 4) * 0.03;
  } else if (mood === "done") {
    dy = -Math.abs(Math.sin(frame / 3)) * canvas.height * 0.04;
  } else {
    dy = Math.sin(frame / 8) * canvas.height * 0.017;
  }
  ctx.save();
  ctx.translate(canvas.width / 2, canvas.height / 2 + dy);
  ctx.rotate(rot);
  ctx.scale(scale, scale);
  const sw = f ? f.w : img.naturalWidth;
  const sh = f ? f.h : img.naturalHeight;
  const k = Math.min(canvas.width / sw, canvas.height / sh);
  ctx.drawImage(
    img,
    f ? f.x : 0,
    f ? f.y : 0,
    sw,
    sh,
    (-sw * k) / 2,
    (-sh * k) / 2,
    sw * k,
    sh * k,
  );
  ctx.restore();
}

function drawFrame(
  ctx: CanvasRenderingContext2D,
  canvas: HTMLCanvasElement,
  mood: AgentState,
  frame: number,
  celebrating: boolean,
) {
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  // 1) Pet pack: frames sliced by alpha (pick the row by state, loop within the row)
  if (petPackRows) {
    const row = rowForMood(petPackRows, MOOD_ROWS[mood]);
    const f = row[frame % row.length];
    if (f) {
      drawFrameFit(ctx, canvas, petPackRows.image, f);
      return;
    }
  }
  // 2) User-supplied image. **No longer assumes an 8×9 grid**: a single PNG would be cut into 1/72 transparent fragments,
  //    looking like "uploaded but not displayed". Now slicing by alpha gaps; only a genuinely multi-frame
  //    sheet plays as "pick row by state + loop within row", otherwise draw whole + state animation.
  if (petSheetFrames && petSheetImg) {
    const rows = petSheetFrames;
    const isSheet = rows.length > 1 || (rows[0]?.length ?? 0) > 1;
    if (isSheet) {
      const row = rows[MOOD_ROWS[mood] % rows.length] ?? rows[0];
      const f = row[frame % row.length];
      if (f) {
        drawFrameFit(ctx, canvas, petSheetImg, f);
        return;
      }
    } else if (rows[0]?.[0]) {
      drawStill(ctx, canvas, petSheetImg, rows[0][0], mood, frame, celebrating);
      return;
    }
  }
  // 3) Built-in logo (whole image)
  if (drawLogo(ctx, canvas, mood, frame, celebrating)) return;
  const cx = canvas.width / 2;
  const cy = canvas.height / 2;
  const bounce = celebrating ? Math.abs(Math.sin(frame)) * 10 : Math.sin(frame / 4) * 2;
  ctx.fillStyle = MOOD_COLORS[mood];
  const size = celebrating ? 34 : 24;
  ctx.fillRect(cx - size / 2, cy - size / 2 - bounce, size, size);
  ctx.fillStyle = "#fff";
  ctx.fillRect(cx - size / 2 + 5, cy - size / 2 - bounce + 6, 6, 6);
  ctx.fillRect(cx + size / 2 - 11, cy - size / 2 - bounce + 6, 6, 6);
}

/** Interactive regions: the pet canvas + the visible bubble (the bubble contains clickable choice buttons). */
function hitRects(canvas: HTMLCanvasElement): DOMRect[] {
  // In 3D mode #pet is display:none (all-zero rect), so use the canvas that is actually shown
  const shown = pet3d ? document.getElementById("pet3d") : null;
  const rects = [(shown ?? canvas).getBoundingClientRect()];
  const bubbleEl = document.getElementById("bubble");
  if (bubbleEl) {
    const r = bubbleEl.getBoundingClientRect();
    if (r.width > 0 && r.height > 0) rects.push(r);
  }
  return rects;
}

export function enableDrag(canvas: HTMLCanvasElement): void {
  const startDrag = () => {
    petAnchor = null; // the user is moving the pet; re-record its position on the next side switch
    void getCurrentWindow().startDragging().catch(() => undefined);
  };
  // 2D / 3D are two canvases (one canvas can only have one context); whichever is shown must be draggable
  canvas.addEventListener("mousedown", startDrag);
  document.getElementById("pet3d")?.addEventListener("mousedown", startDrag);
  let ignoring: boolean | null = null;
  const applyIgnore = (want: boolean) => {
    if (want === ignoring) return;
    ignoring = want;
    void getCurrentWindow().setIgnoreCursorEvents(want).catch(() => undefined);
  };
  // Cursor position is pushed by Rust after polling: once the window is click-through it receives no mousemove, so it must be driven externally.
  applyIgnore(true);
  void listen<{ x: number; y: number }>("pet-cursor", (ev) => {
    const { x, y } = ev.payload;
    const petRect = hitRects(canvas)[0];
    // Normalize for 3D: VRM looks at the cursor (NDC, -1..1, y up)
    if (petRect && petRect.width > 0) {
      pet3d?.setCursor(
        ((x - petRect.left) / petRect.width) * 2 - 1,
        1 - ((y - petRect.top) / petRect.height) * 2,
      );
    }
    const over = hitRects(canvas).some(
      (r) => x >= r.left && x <= r.right && y >= r.top && y <= r.bottom,
    );
    applyIgnore(!over);
  });
}

/** Apply pet.state to the state badge + default message. With no session, give the pet a fallback hint. */
function applyStateBadge(
  state: PetState,
  payload: { state?: unknown; fromEvent?: unknown; agent?: unknown } = {},
): void {
  const badge = document.getElementById("state-badge");
  const label = document.getElementById("state-label");
  if (!badge || !label) return;
  const cls = PET_STATE_CLASS[state];
  for (const c of VALID_PET_STATES) badge.classList.remove(PET_STATE_CLASS[c]);
  badge.classList.add(cls);
  badge.classList.add("visible");
  label.textContent = t(PET_STATE_I18N[state]);
  // On state change: write the default message into the session list, going through the bubble render path (consistent style).
  lastActivity = Date.now();
  const fromEvent = String(payload.fromEvent ?? "");
  const agent = String(payload.agent ?? "opencapx");
  const msg = t(PET_STATE_I18N[state]);
  sessions = mergeSession(sessions, {
    id: `pet-state-${fromEvent || "now"}-${Date.now()}`,
    agent,
    project: "",
    message: msg,
    state: PET_STATE_MAP[state] ?? "idle",
    updatedAt: Math.floor(Date.now() / 1000),
  });
}

export async function startOverlay(canvas: HTMLCanvasElement): Promise<void> {
  const prevStates: Record<string, AgentState> = {};
  let celebrateUntil = 0;
  const ctx = canvas.getContext("2d");
  if (!ctx) return;

  await loadPrefs();
  applyPetSize(canvas);
  // Apply the saved side right away: index.html hardcodes data-bpos="right", so without this the
  // pet window shows the wrong layout for a second and then visibly jumps on the first poll tick.
  await applyBubblePos();
  await syncWindowHeight();
  try {
    const saved = await invoke<Record<string, unknown>>("get_settings");
    if (saved.onboarded === false) {
      await invoke("open_settings").catch(() => undefined);
    }
  } catch {
    /* ignore */
  }

  const bubbleEl = document.getElementById("bubble");
  // The git branch / short path the group header needs. Dedupe by the cwd set and ask Core only when the set changes
  // (the bubble re-renders every second, so we cannot invoke every time).
  let projectMeta = new Map<string, ProjectMeta>();
  let metaKey = "";
  const refreshProjectMeta = async (list: AgentEvent[]): Promise<void> => {
    const cwds = [...new Set(list.map((s) => (s.cwd ?? "").trim()).filter((c) => c !== ""))].sort();
    const key = cwds.join("\n");
    if (key === metaKey) return;
    metaKey = key;
    if (cwds.length === 0) {
      projectMeta = new Map();
      return;
    }
    try {
      const rows = await invoke<ProjectMeta[]>("project_meta", { cwds });
      projectMeta = new Map(rows.map((m) => [m.cwd, m]));
    } catch {
      // Branch is enrichment: when unavailable, show only the project name; row rendering is unaffected
      projectMeta = new Map();
    }
  };
  const paintBubble = () => {
    // Yield when there is a pending confirmation Prompt (ask / permission / install): otherwise this per-second re-render
    // wipes out the confirmation UI that renderAsk appended into #bubble — the user cannot click and can only wait for the timeout.
    if (askBox) {
      if (askBox.isConnected) return;
      askBox = null;
    }
    const now = Date.now();
    sessions = sessions.filter(
      (s) =>
        (s.state !== "done" || now - s.updatedAt * 1000 <= 30000) &&
        (!s.id.startsWith("say-") || now - s.updatedAt * 1000 <= 60000),
    );
    if (expandedRows.size > 0) {
      const live = new Set(sessions.map((s) => s.id));
      for (const id of expandedRows) if (!live.has(id)) expandedRows.delete(id);
    }
    if (!prefs.bubbleEnabled) {
      if (bubbleEl && bubbleEl.innerHTML !== "") bubbleEl.innerHTML = "";
      return;
    }
    if (prefs.bubbleDuration > 0 && sessions.length > 0 && now - lastActivity > prefs.bubbleDuration * 1000) {
      if (bubbleEl && bubbleEl.innerHTML !== "") bubbleEl.innerHTML = "";
      return;
    }
    if (bubbleEl) {
      void refreshProjectMeta(sessions);
      renderBubble(bubbleEl, sessions, {
        mode: prefs.mode,
        theme: prefs.theme,
        maxRows: prefs.maxRows,
        density: prefs.density,
        projects: projectMeta,
        expandedIds: expandedRows,
      });
      bindRowToggle(bubbleEl);
    }
  };

  /** Attach "click to expand/collapse" to clipped rows. renderBubble re-attaches it on every re-render. */
  const bindRowToggle = (el: HTMLElement): void => {
    el.querySelectorAll<HTMLElement>(".row").forEach((row) => {
      const id = row.dataset.rowSid ?? "";
      const msg = row.querySelector<HTMLElement>(".msg");
      if (!id || !msg) return;
      const open = expandedRows.has(id);
      // Nothing clipped means no need to expand (scrollWidth shrinks when expanded, so open is the fallback).
      // The body line counts too — sometimes it is the one clipped.
      const speech = row.querySelector<HTMLElement>(".speech");
      const clipped =
        msg.scrollWidth > msg.clientWidth + 1 ||
        (!!speech && speech.scrollWidth > speech.clientWidth + 1);
      if (!open && !clipped) return;
      row.classList.add("expandable");
      row.addEventListener("click", (ev) => {
        if ((ev.target as HTMLElement).closest("button")) return; // do not steal clicks from choice buttons
        if (open) expandedRows.delete(id);
        else expandedRows.add(id);
        paintBubble();
      });
    });
  };
  try {
    sessions = await invoke<AgentEvent[]>("get_sessions");
    for (const s of sessions) prevStates[s.id] = s.state;
    if (sessions.length > 0) lastActivity = Date.now();
  } catch {
    sessions = [];
  }
  paintBubble();
  void invoke("ui_ping", { count: sessions.length, lastState: "boot" }).catch(() => undefined);


  await onAgentEvent((e) => {
    const prev = prevStates[e.id];
    prevStates[e.id] = e.state;
    lastActivity = Date.now();
    sessions = mergeSession(sessions, e);
    if (e.state === "done") celebrateUntil = Date.now() + 3000;
    soundForTransition(prev, e.state);
    paintBubble();
    void invoke("ui_ping", { count: sessions.length, lastState: e.state }).catch(() => undefined);
  });

  // MCP tool chain: opencapx.say / set_state / ask (see docs/mcp.md)
  void listen<{ text: string }>("opencapx-say", (ev) => {
    lastActivity = Date.now();
    sessions = mergeSession(sessions, {
      id: `say-${Date.now()}`,
      agent: "opencapx",
      project: "",
      message: ev.payload.text,
      state: "idle",
      updatedAt: Math.floor(Date.now() / 1000),
    });
    paintBubble();
  });

  void listen<{ state: string; message?: string }>("opencapx-set-state", (ev) => {
    manualMood = { state: PET_STATE_MAP[ev.payload.state] ?? "idle", until: Date.now() + 15000 };
    if (ev.payload.message) {
      lastActivity = Date.now();
      sessions = mergeSession(sessions, {
        id: `say-${Date.now()}`,
        agent: "opencapx",
        project: "",
        message: ev.payload.message,
        state: "idle",
        updatedAt: Math.floor(Date.now() / 1000),
      });
    }
    paintBubble();
  });

  void listen<{
    id: string;
    question: string;
    options: string[];
    multi?: boolean;
    fields?: AskField[];
    timeout: number;
  }>("opencapx-ask", (ev) => {
    lastActivity = Date.now();
    renderAsk(bubbleEl, {
      id: ev.payload.id,
      question: ev.payload.question,
      options: (ev.payload.options ?? []).map((s) => ({ label: s, value: s })),
      multi: ev.payload.multi === true,
      fields: ev.payload.fields,
    });
  });

  void listen<{ id: string; answer: string | null }>("opencapx-ask-done", () => {
    clearAsk();
  });

  void listen<{ id: string; pluginId: string; permission: string; canAlways: boolean; reason?: string }>(
    "opencapx-permission-ask",
    (ev) => {
      lastActivity = Date.now();
      const opts = [{ label: t("permAllowOnce"), value: "once" }, { label: t("permDeny"), value: "deny" }];
      // v1.5 session scope: between once and always, in process memory, cleared on restart
      opts.splice(1, 0, { label: t("permAllowSession"), value: "session" });
      if (ev.payload.canAlways) opts.splice(2, 0, { label: t("permAlways"), value: "always" });
      const reasonLine = ev.payload.reason
        ? `\n${t("permReason")}: ${ev.payload.reason}`
        : "";
      renderAsk(bubbleEl, {
        id: ev.payload.id,
        question: `${ev.payload.pluginId} ${t("permWants")} ${ev.payload.permission}${reasonLine}`,
        options: opts,
        command: "answer_permission",
      });
    },
  );

  void listen<{ id: string; answer: string | null }>("opencapx-permission-ask-done", () => {
    clearAsk();
  });

  // Agent-layer permission gate (docs/permissions.md "two-layer decision"): the subject is an AgentIdentity
  // rather than a plugin. Answers go through the same answer_permission (the asks registry is shared).
  void listen<{ id: string; agentId: string; displayName: string; permission: string; canAlways: boolean }>(
    "opencapx-agent-permission-ask",
    (ev) => {
      lastActivity = Date.now();
      const opts = [{ label: t("permAllowOnce"), value: "once" }, { label: t("permDeny"), value: "deny" }];
      // v1.5 session scope: between once and always, in process memory, cleared on restart
      opts.splice(1, 0, { label: t("permAllowSession"), value: "session" });
      if (ev.payload.canAlways) opts.splice(2, 0, { label: t("permAlways"), value: "always" });
      renderAsk(bubbleEl, {
        id: ev.payload.id,
        question: `${ev.payload.displayName} ${t("permWants")} ${ev.payload.permission}`,
        options: opts,
        command: "answer_permission",
      });
    },
  );

  void listen<{ id: string; answer: string | null }>("opencapx-agent-permission-ask-done", () => {
    clearAsk();
  });

  // SSE live stream (SSE is a redundant path inside Tauri — Tauri's built-in event channel is more reliable;
  // this is mainly for plain browsers / debug panels, and lets the audit page consume the event stream directly).
  connectEventStream();
  void onEvent("pet.bubble", (ev) => {
    const text = String((ev.payload as { text?: unknown })?.text ?? "");
    if (!text) return;
    lastActivity = Date.now();
    sessions = mergeSession(sessions, {
      id: `sse-say-${ev.timestamp}`,
      agent: "opencapx",
      project: "",
      message: text,
      state: "idle",
      updatedAt: ev.timestamp,
    });
    paintBubble();
  });
  void onEvent("pet.state", (ev) => {
    const raw = String((ev.payload as { state?: unknown })?.state ?? "");
    if (!raw) return;
    const valid = VALID_PET_STATES.has(raw as PetState) ? (raw as PetState) : "idle";
    manualMood = { state: PET_STATE_MAP[valid] ?? "idle", until: Date.now() + 15000 };
    applyStateBadge(valid, ev.payload as { state?: unknown; fromEvent?: unknown; agent?: unknown });
    paintBubble();
  });

  void listen<{ id: string; pluginId: string; permission: string; canAlways: boolean }>(
    "opencapx-install-ask",
    (ev) => {
      lastActivity = Date.now();
      const opts = [{ label: t("permAllowOnce"), value: "once" }, { label: t("permDeny"), value: "deny" }];
      if (ev.payload.canAlways) opts.splice(1, 0, { label: t("permAlways"), value: "always" });
      renderAsk(bubbleEl, {
        id: ev.payload.id,
        question: `${t("installConfirm")} ${ev.payload.pluginId} → ${ev.payload.permission}`,
        options: opts,
        command: "answer_install",
      });
    },
  );

  void listen<{ id: string; answer: string | null }>("opencapx-install-ask-done", () => {
    clearAsk();
  });

  paintBubble();
  let petShown = true;
  window.setInterval(() => {
    paintBubble();
    void loadPrefs().then(async () => {
      applyPetSize(canvas);
      // Side switch first (full anchor dance), then the petSize sync: when the side did change,
      // applyBubblePos already applied the matching height and the sync becomes a no-op.
      await applyBubblePos();
      await syncWindowHeight();
      if (prefs.petVisible !== petShown) {
        petShown = prefs.petVisible;
        const win = getCurrentWindow();
        if (petShown) void win.show().catch(() => undefined);
        else void win.hide().catch(() => undefined);
      }
      const nowMs = Date.now();
      const busy = sessions.some((s) => s.state === "working" || s.state === "waiting");
      if (!prefs.breakEnabled || !busy) {
        activeSince = 0;
        return;
      }
      if (activeSince === 0) {
        activeSince = nowMs;
        return;
      }
      if (nowMs - activeSince >= prefs.breakMinutes * 60000) {
        activeSince = nowMs;
        try {
          sendNotification({ title: "OpenCapX", body: t("breakMessage") });
        } catch {
          /* notifications unavailable */
        }
      }
    });
  }, 1000);

  let last = 0;
  let frame = 0;
  const tick = (t: number) => {
    const mood =
      manualMood && Date.now() < manualMood.until ? manualMood.state : aggregate(sessions);
    const celebrating = Date.now() < celebrateUntil;
    // 3D: states are handed to AnimationMixer cross-fades; maintain our own 30fps render loop
    if (pet3d) {
      pet3d.setMood(mood as Mood3D);
      setPet3dRunning(prefs.petVisible);
      ctx.clearRect(0, 0, canvas.width, canvas.height);
      requestAnimationFrame(tick);
      return;
    }
    if (t - last > 1000 / FPS[mood]) {
      drawFrame(ctx, canvas, mood, frame, celebrating);
      frame += 1;
      last = t;
    }
    requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
  // Pause 3D rendering when the document/window is invisible (the persistent window's power-saving switch)
  document.addEventListener("visibilitychange", () => setPet3dRunning(prefs.petVisible));
  enableDrag(canvas);
}

const canvas = document.getElementById("pet") as HTMLCanvasElement | null;
void requestPermission().catch(() => undefined);
if (canvas) {
  void startOverlay(canvas);
}










