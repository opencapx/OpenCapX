import { invoke } from "@tauri-apps/api/core";
import { t } from "../i18n";
import { startHotkeyPaletteListener } from "./modals";
import { esc, group } from "./shared";
import type { HotkeyAction, PaletteEntry } from "./types";

// ---- Phase 39 — Hotkeys tab (global hotkeys + command palette) ------------------------

export function renderHotkeys(body: HTMLElement): void {
  body.innerHTML = group("tabHotkeys",
    `<div class="setting-row vertical"><span class="setting-hint">${esc(t("hotkeysHint"))}</span><div><button class="btn ghost" id="hotkey-restore-defaults" type="button">${esc(t("hotkeyRestoreDefaults"))}</button><button class="btn ghost" id="hotkey-refresh" type="button">${esc(t("auditRefresh"))}</button><span class="setting-hint" id="hotkey-msg"></span></div><div id="hotkey-list"></div></div>`);
  document.getElementById("hotkey-restore-defaults")?.addEventListener("click", () => void restoreDefaultHotkeys());
  document.getElementById("hotkey-refresh")?.addEventListener("click", () => void refreshHotkeys());
  void refreshHotkeys();
  startHotkeyPaletteListener();
}

interface HotkeyBinding {
  combo: string;
  action: HotkeyAction;
  enabled: boolean;
  registered_at_os: boolean;
}

interface HotkeyResult {
  binding: HotkeyBinding;
  registered: boolean;
  error?: string;
}

const BUILTIN_ACTION_OPTIONS: Array<{ key: string; i18nKey: string }> = [
  { key: "toggle-pet", i18nKey: "hotkeyActionTogglePet" },
  { key: "open-settings", i18nKey: "hotkeyActionOpenSettings" },
  { key: "open-palette", i18nKey: "hotkeyActionOpenPalette" },
  { key: "quit", i18nKey: "hotkeyActionQuit" },
];

function formatHotkeyAction(a: HotkeyAction): string {
  if (a.kind === "builtin") {
    const opt = BUILTIN_ACTION_OPTIONS.find((o) => o.key === a.action);
    return opt ? t(opt.i18nKey as never) : (a.action ?? "?");
  }
  return `${a.plugin_id} → ${a.capability}`;
}

/// Current list snapshot: the rebind guard, overwrite confirmation, and restore-default prompt all rely on it; updated on refreshHotkeys.
let currentBindings: HotkeyBinding[] = [];

/// Aligned with the backend normalize_combo (for the frontend guard: BTreeSet lexicographic order Alt < CmdOrCtrl < Shift < Super).
/// `'cmdorctrl'` catches an already-normalized string (idempotent): the picker and restore-default feed normalized strings back in, and without it you get garbage like Shift+CMDORCTRL+P.
function normalizeComboTs(raw: string): string {
  const mods = new Set<string>();
  const keys: string[] = [];
  for (const part of raw.split("+")) {
    const p = part.trim();
    if (!p) continue;
    const low = p.toLowerCase();
    if (["ctrl", "control", "cmd", "command", "cmdorctrl"].includes(low)) mods.add("CmdOrCtrl");
    else if (low === "alt" || low === "option") mods.add("Alt");
    else if (low === "shift") mods.add("Shift");
    else if (["super", "meta", "win", "windows"].includes(low)) mods.add("Super");
    else keys.push(p.toUpperCase());
  }
  keys.sort();
  return [...[...mods].sort(), ...keys].join("+");
}

const IS_MAC = typeof navigator !== "undefined" && /Mac|iP(hone|ad|od)/.test(navigator.userAgent);

/// Presentation polish: storage is always the normalized string; only the display is swapped to platform-conventional symbols.
function prettifyCombo(combo: string): string {
  return combo
    .split("+")
    .map((tok) => {
      if (tok === "CmdOrCtrl") return IS_MAC ? "⌘" : "Ctrl";
      if (tok === "Shift") return "⇧";
      if (tok === "Alt") return IS_MAC ? "⌥" : "Alt";
      return tok;
    })
    .join("+");
}

function sameAction(a: HotkeyAction | undefined, b: HotkeyAction | undefined): boolean {
  if (!a || !b || a.kind !== b.kind) return false;
  return a.kind === "builtin"
    ? a.action === b.action
    : a.plugin_id === b.plugin_id && a.capability === b.capability;
}

function setHotkeyMsg(text: string): void {
  const m = document.getElementById("hotkey-msg");
  if (m) m.textContent = text;
}

function reportSetResult(combo: string, r: HotkeyResult): void {
  if (r.registered) setHotkeyMsg(`✓ ${prettifyCombo(combo)}`);
  else if (r.error) setHotkeyMsg(`⚠ ${t("hotkeySavedNotActive")}: ${r.error}`);
  else setHotkeyMsg(`⚠ ${t("hotkeyNotActiveHint")}`);
}

/// Recorder key normalization: fixes arrow keys / Shift+digits / named-key casing; returns null = cannot map.
const NAMED_KEYS: Record<string, string> = {
  ArrowUp: "Up",
  ArrowDown: "Down",
  ArrowLeft: "Left",
  ArrowRight: "Right",
  " ": "Space",
  Tab: "Tab",
  Enter: "Enter",
  Backspace: "Backspace",
  Delete: "Delete",
  Home: "Home",
  End: "End",
  PageUp: "PageUp",
  PageDown: "PageDown",
  Insert: "Insert",
};
const F_KEY_RE = /^F([1-9]|1[0-2])$/;

function keyNameForCombo(ev: KeyboardEvent): { key: string; isFKey: boolean } | null {
  if (ev.isComposing) return null;
  const k = ev.key;
  if (NAMED_KEYS[k] !== undefined) return { key: NAMED_KEYS[k], isFKey: false };
  if (F_KEY_RE.test(k)) return { key: k, isFKey: true };
  if (k.length === 1 && /[a-z]/i.test(k)) return { key: k.toUpperCase(), isFKey: false };
  // Shift+digit: key is something like '!', but code is still Digit1 — use code to recover the digit key.
  const codeMatch = /^(?:Digit|Numpad)([0-9])$/.exec(ev.code);
  if (codeMatch) return { key: codeMatch[1], isFKey: false };
  return null; // punctuation / dead keys / other layout chars: unsupported
}

/// Recorder core: keyboard event -> modifier progress / unsupported / missing modifier / combo (raw, not normalized).
/// Rules are exactly the same as the original dialog recorder (a modifier pressed alone is not recorded; F1-F12 may have no modifier, everything else needs at least one),
/// only the consumer changed from a dialog to an inline pill.
type HotkeyCaptureOutcome =
  | { kind: "modifier"; mods: string[] }
  | { kind: "unsupported" }
  | { kind: "need-modifier" }
  | { kind: "combo"; combo: string };

function comboFromKeyEvent(ev: KeyboardEvent): HotkeyCaptureOutcome {
  const mods: string[] = [];
  if (ev.ctrlKey || ev.metaKey) mods.push("Ctrl");
  if (ev.altKey) mods.push("Alt");
  if (ev.shiftKey) mods.push("Shift");
  // A modifier pressed alone is not recorded, only the progress display updates
  if (["Control", "Shift", "Alt", "Meta"].includes(ev.key)) return { kind: "modifier", mods };
  const mapped = keyNameForCombo(ev);
  if (!mapped) return { kind: "unsupported" };
  // F1-F12 may have no modifier; everything else requires at least one modifier
  if (mods.length === 0 && !mapped.isFKey) return { kind: "need-modifier" };
  return { kind: "combo", combo: [...mods, mapped.key].join("+") };
}

/// Row data: palette action + display label; the binding is looked up on the fly from currentBindings by sameAction.
interface HotkeyRow {
  action: HotkeyAction;
  label: string;
}

/// Render item: palette row / orphan row; orphan rows carry their source binding, and the binding a palette row claims is also recorded here.
interface HotkeyItem {
  row: HotkeyRow;
  orphan: boolean;
  binding?: HotkeyBinding;
}

/// Teardown function for the pill being recorded (at most one at a time); refresh calls it first to avoid stale keydown listeners.
let endHotkeyCapture: (() => void) | null = null;

function cancelHotkeyCapture(): void {
  endHotkeyCapture?.();
}

async function refreshHotkeys(): Promise<void> {
  const box = document.getElementById("hotkey-list");
  if (!box) return;
  cancelHotkeyCapture();
  try {
    currentBindings = await invoke<HotkeyBinding[]>("list_hotkeys");
  } catch (err) {
    box.innerHTML = `<p class="setting-hint">✗ ${esc(String(err))}</p>`;
    return;
  }
  let entries: PaletteEntry[] = [];
  try {
    entries = await invoke<PaletteEntry[]>("list_palette_entries");
  } catch {
    entries = [];
  }
  // Row = palette LEFT JOIN bindings (matched by sameAction): builtins in fixed order, plugins by plugin -> capability.
  const builtinRank = (action: HotkeyAction): number => {
    const i = BUILTIN_ACTION_OPTIONS.findIndex((o) => o.key === action.action);
    return i < 0 ? BUILTIN_ACTION_OPTIONS.length : i;
  };
  const toRow = (e: PaletteEntry): HotkeyRow => {
    // A builtin's action is the `builtin:` suffix of its id (aligned with the backend BuiltinAction's kebab-case).
    const action: HotkeyAction =
      e.kind === "builtin"
        ? { kind: "builtin", action: e.id.replace(/^builtin:/, "") as HotkeyAction["action"] }
        : { kind: "plugin", plugin_id: e.plugin_id ?? "", capability: e.capability ?? "" };
    // builtin labels go through i18n; plugins use the palette title (the backend already assembles `plugin -> capability`).
    return { action, label: e.kind === "builtin" ? formatHotkeyAction(action) : e.title };
  };
  const rows = [
    ...entries
      .filter((e) => e.kind === "builtin")
      .map(toRow)
      .sort((a, b) => builtinRank(a.action) - builtinRank(b.action)),
    ...entries
      .filter((e) => e.kind === "plugin")
      .map(toRow)
      .sort(
        (a, b) =>
          (a.action.plugin_id ?? "").localeCompare(b.action.plugin_id ?? "") ||
          (a.action.capability ?? "").localeCompare(b.action.capability ?? ""),
      ),
  ];
  // Row = palette LEFT JOIN bindings; each palette row claims the first binding with the same action.
  // Unclaimed bindings (orphans after a plugin uninstall, or extra combos with the same action in old data) are appended after the separator.
  const claimed = new Set<HotkeyBinding>();
  const items: HotkeyItem[] = rows.map((row) => {
    const binding = currentBindings.find((x) => !claimed.has(x) && sameAction(x.action, row.action));
    if (binding) claimed.add(binding);
    return { row, orphan: false, binding };
  });
  const orphans = currentBindings.filter((b) => !claimed.has(b));
  items.push(
    ...orphans.map((b) => ({
      row: { action: b.action, label: formatHotkeyAction(b.action) },
      orphan: true,
      binding: b,
    })),
  );

  const rowHtml = (item: HotkeyItem, index: number): string => {
    const { row, orphan, binding: b } = item;
    const notActive = !!b && b.enabled && !b.registered_at_os;
    const pillClass = `hotkey-pill${b ? " bound" : ""}${notActive ? " warn" : ""}`;
    const pillText = b ? prettifyCombo(b.combo) : t("hotkeyNoBinding");
    const pillTitle = notActive ? t("hotkeyNotActiveHint") : b ? t("hotkeyRebind") : t("hotkeyPressKeys");
    const pill = `<button class="${pillClass}" data-hotkey-row="${index}" data-combo="${esc(b?.combo ?? "")}" title="${esc(pillTitle)}" type="button">${esc(pillText)}</button>`;
    const toggle = b
      ? `<button class="btn ghost" data-hotkey-toggle="${esc(b.combo)}" data-next="${b.enabled ? "0" : "1"}" type="button">${esc(b.enabled ? t("hotkeyDisable") : t("hotkeyEnable"))}</button>`
      : "";
    let badge = "";
    if (b && b.enabled && !b.registered_at_os) {
      badge = `<span class="hotkey-badge">${esc(t("hotkeyNotActive"))}</span><button class="btn ghost" data-hotkey-retry="${esc(b.combo)}" type="button">${esc(t("hotkeyRetry"))}</button>`;
    }
    // Orphan rows keep an explicit delete button (no palette action to lean on); normal rows are cleared with the pill's Delete.
    const del =
      orphan && b
        ? `<button class="btn ghost danger" data-hotkey-del="${esc(b.combo)}" type="button">${esc(t("remove"))}</button>`
        : "";
    return `<div class="sess hotkey-row${b && !b.enabled ? " inactive" : ""}" data-hotkey-combo="${esc(b?.combo ?? "")}"><span class="hotkey-label">${esc(row.label)}</span><span class="hotkey-actions">${pill}${toggle}${badge}${del}</span></div>`;
  };

  const divider = `<div class="hotkey-orphan-divider"><span>${esc(t("hotkeyOrphaned"))}</span></div>`;
  box.innerHTML =
    items.length === 0
      ? `<p class="empty">${esc(t("hotkeysNone"))}</p>`
      : items
          .map((item, i) => (i === rows.length && orphans.length > 0 ? divider : "") + rowHtml(item, i))
          .join("");

  box.querySelectorAll<HTMLButtonElement>("button.hotkey-pill").forEach((pill) => {
    pill.addEventListener("click", () => {
      const item = items[Number(pill.dataset.hotkeyRow ?? "-1")];
      if (!item) return;
      beginHotkeyCapture(pill, item.row, item.binding?.combo ?? "");
    });
  });
  box.querySelectorAll<HTMLButtonElement>("button[data-hotkey-del]").forEach((btn) => {
    btn.addEventListener("click", () => void deleteHotkeyBinding(btn.dataset.hotkeyDel ?? ""));
  });
  box.querySelectorAll<HTMLButtonElement>("button[data-hotkey-toggle]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const combo = btn.dataset.hotkeyToggle ?? "";
      const enabled = btn.dataset.next === "1";
      try {
        const r = await invoke<HotkeyResult>("set_hotkey_enabled", { combo, enabled });
        reportSetResult(combo, r);
      } catch (err) {
        setHotkeyMsg(`✗ ${String(err)}`);
      }
      await refreshHotkeys();
    });
  });
  box.querySelectorAll<HTMLButtonElement>("button[data-hotkey-retry]").forEach((btn) => {
    btn.addEventListener("click", () => void retryHotkey(btn.dataset.hotkeyRetry ?? ""));
  });
}

/// Clear a binding (normal rows via the pill's Delete/Backspace, orphan rows via the delete button).
async function deleteHotkeyBinding(combo: string): Promise<void> {
  if (!combo) return;
  try {
    await invoke("delete_hotkey", { combo });
  } catch (err) {
    setHotkeyMsg(`✗ ${String(err)}`);
  }
  await refreshHotkeys();
}

/// Write the recorded combo to the row's action: confirm first if it collides with another action (after overwriting, the other becomes unbound);
/// if the combo changed, delete the old combo so an action keeps only one binding.
async function saveHotkeyBinding(row: HotkeyRow, combo: string, oldCombo: string): Promise<void> {
  const clash = currentBindings.find((b) => b.combo === combo && !sameAction(b.action, row.action));
  if (clash && !window.confirm(t("hotkeyOverwriteConfirm"))) {
    await refreshHotkeys();
    return;
  }
  try {
    const r = await invoke<HotkeyResult>("set_hotkey", { combo, action: row.action });
    reportSetResult(r.binding.combo, r);
    if (oldCombo && oldCombo !== combo) {
      await invoke("delete_hotkey", { combo: oldCombo });
    }
  } catch (err) {
    setHotkeyMsg(`✗ ${String(err)}`);
  }
  await refreshHotkeys();
}

/// Inline pill recording state: Esc cancels, Delete/Backspace clears, valid combos go to saveHotkeyBinding.
function beginHotkeyCapture(pill: HTMLButtonElement, row: HotkeyRow, oldCombo: string): void {
  cancelHotkeyCapture();
  const restingText = pill.textContent ?? "";
  let restoreTimer: number | undefined;
  const paint = (text: string) => {
    pill.textContent = text;
  };
  const end = () => {
    window.removeEventListener("keydown", onKey, true);
    if (restoreTimer !== undefined) window.clearTimeout(restoreTimer);
    restoreTimer = undefined;
    pill.classList.remove("capturing");
    paint(restingText);
    if (endHotkeyCapture === end) endHotkeyCapture = null;
  };
  // Unsupported / missing modifier: a brief hint inside the pill, returning to the recording prompt after 1.2s.
  const showTransient = (text: string) => {
    paint(text);
    if (restoreTimer !== undefined) window.clearTimeout(restoreTimer);
    restoreTimer = window.setTimeout(() => paint(t("hotkeyPressKeys")), 1200);
  };
  const onKey = (ev: KeyboardEvent) => {
    ev.preventDefault();
    ev.stopPropagation();
    if (ev.key === "Escape") {
      end();
      return;
    }
    if (ev.key === "Delete" || ev.key === "Backspace") {
      end();
      void deleteHotkeyBinding(oldCombo);
      return;
    }
    const outcome = comboFromKeyEvent(ev);
    if (outcome.kind === "modifier") {
      paint(outcome.mods.length ? `${outcome.mods.join("+")}+…` : t("hotkeyPressKeys"));
      return;
    }
    if (outcome.kind === "unsupported") {
      showTransient(t("hotkeyUnsupportedKey"));
      return;
    }
    if (outcome.kind === "need-modifier") {
      showTransient(t("hotkeyRecordNeedModifier"));
      return;
    }
    end();
    void saveHotkeyBinding(row, normalizeComboTs(outcome.combo), oldCombo);
  };
  pill.classList.add("capturing");
  paint(t("hotkeyPressKeys"));
  endHotkeyCapture = end;
  window.addEventListener("keydown", onKey, true);
}

/// Retry registration = call set_hotkey again with the current action (idempotent upsert + re-register).
async function retryHotkey(combo: string): Promise<void> {
  const b = currentBindings.find((x) => x.combo === combo);
  if (!b) return;
  try {
    const r = await invoke<HotkeyResult>("set_hotkey", { combo, action: b.action });
    reportSetResult(r.binding.combo, r);
  } catch (err) {
    setHotkeyMsg(`✗ ${String(err)}`);
  }
  await refreshHotkeys();
}

const DEFAULT_HOTKEYS: Array<[string, HotkeyAction]> = [
  ["CmdOrCtrl+Shift+P", { kind: "builtin", action: "open-palette" }],
  ["CmdOrCtrl+Shift+,", { kind: "builtin", action: "open-settings" }],
  ["CmdOrCtrl+Shift+H", { kind: "builtin", action: "toggle-pet" }],
];

/// Restore default: upserts only these three combos; confirm first if non-default actions are already bound.
/// NOTE: default items the user disabled are re-enabled (upsert writes enabled:true); expected behavior.
async function restoreDefaultHotkeys(): Promise<void> {
  const overwritten = currentBindings.filter((b) => {
    const d = DEFAULT_HOTKEYS.find(([c]) => normalizeComboTs(c) === b.combo);
    return d && !sameAction(d[1], b.action);
  });
  if (overwritten.length > 0 && !window.confirm(t("hotkeyRestoreConfirm"))) return;
  for (const [combo, action] of DEFAULT_HOTKEYS) {
    try {
      const r = await invoke<HotkeyResult>("set_hotkey", { combo, action });
      reportSetResult(combo, r);
    } catch (err) {
      setHotkeyMsg(`✗ ${combo}: ${String(err)}`);
    }
  }
  await refreshHotkeys();
}
