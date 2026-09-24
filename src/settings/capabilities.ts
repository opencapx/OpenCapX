import { invoke } from "@tauri-apps/api/core";
import { t, type I18nKey } from "../i18n";
import { esc, group, listError, listSkeleton } from "./shared";

export function renderCapabilities(body: HTMLElement): void {
  body.innerHTML = group("tabCapabilities", capLegend())
    + `<div id="core-perm-list"></div>`;
  document.getElementById("core-perm-refresh")?.addEventListener("click", () => void refreshCorePerms());
  void refreshCorePerms(null, true);
}

/// Global policy entry (core_permission_list). Control granularity = permission, not capability.
interface CorePermPolicy {
  permission: string;
  capabilities: string[];
  builtinDefault: string;
  /// null = follow the built-in default
  override: string | null;
  effective: string;
  highRisk: boolean;
}

/// Global policy category table (array order = render order); permissions not listed fall into capCatOther, rendered last.
const CORE_PERM_CATEGORIES: Array<{ key: string; titleKey: I18nKey; permissions: string[] }> = [
  { key: "clipboard", titleKey: "capCatClipboard", permissions: ["clipboard.read", "clipboard.write"] },
  { key: "file", titleKey: "capCatFile", permissions: ["file.read", "filesystem.write"] },
  { key: "image", titleKey: "capCatImage", permissions: ["image.read"] },
  { key: "screen", titleKey: "capCatScreen", permissions: ["screen.capture"] },
  { key: "audio", titleKey: "capCatAudio", permissions: ["notification.post", "audio.output", "media.control"] },
  { key: "browser", titleKey: "capCatBrowser", permissions: ["browser.control"] },
  { key: "automation", titleKey: "capCatAutomation", permissions: ["automation.control", "input.control"] },
  { key: "pim", titleKey: "capCatPim", permissions: ["photos.read", "contacts.read", "calendar.read", "notes.read", "reminders.read", "reminders.write", "mail.read", "messages.read"] },
  { key: "location", titleKey: "capCatLocation", permissions: ["location.read"] },
  { key: "system", titleKey: "capCatSystem", permissions: ["system.settings", "power.control", "url.scheme.open"] },
  { key: "printer", titleKey: "capCatPrinter", permissions: ["printer.control"] },
  { key: "window", titleKey: "capCatWindow", permissions: ["window.management"] },
  { key: "things", titleKey: "capCatThings", permissions: ["things.read", "things.write"] },
];

/// Backend decision enum -> localized text (granted/ask/denied); unknown values are returned as-is.
function decisionLabel(v: string): string {
  if (v === "granted") return t("permGranted");
  if (v === "ask") return t("permAsk");
  if (v === "denied") return t("permDenied");
  return v;
}

/// Legend: tier explanation + hard-gate rule (red dot and red text, read as a warning rather than a gray hint) + subscription note + refresh.
function capLegend(): string {
  return `<div class="setting-row vertical cap-legend"><span class="cap-legend-title">${esc(t("corePermHint"))}</span><span class="cap-legend-rule">${esc(t("corePermHardGateHint"))}</span><span class="setting-hint">${esc(t("corePermSubscriptionNote"))}</span><div><button class="btn ghost" id="core-perm-refresh" type="button">${esc(t("auditRefresh"))}</button><span class="setting-hint" id="core-perm-msg"></span></div></div>`;
}

/// Single row: permission name + covered capability chip + built-in default tier + 4-tier native select.
/// High-risk permissions offer no granted option (the backend rejects global granted; this avoids a pointless click).
/// data-override for CSS scanning: '' = follow default; other tiers show a colored bar on the row's left edge;
/// data-cur colors the collapsed select. aria-label carries the permission name, so screen readers don't just say 'follow default'.
function corePermRow(e: CorePermPolicy): string {
  const current = e.override ?? "";
  const opts: Array<[string, I18nKey]> = [["", "corePermFollowDefault"]];
  if (!e.highRisk) opts.push(["granted", "permGranted"]);
  opts.push(["ask", "permAsk"]);
  opts.push(["denied", "permDenied"]);
  const sel = `<select data-cap-perm="${esc(e.permission)}" data-cur="${current}" aria-label="${esc(e.permission)}">${opts
    .map(([v, k]) => `<option value="${esc(v)}"${v === current ? " selected" : ""}>${esc(t(k))}</option>`)
    .join("")}</select>`;
  const chips = e.capabilities.map((c) => `<span class="cap-badge">${esc(c)}</span>`).join("");
  const chipsRow = e.capabilities.length > 0 ? `<span class="cap-chips">${chips}</span>` : "";
  const badge = e.highRisk ? ` <span class="cap-flag">${esc(t("permHighRisk"))}</span>` : "";
  // Rows whose effective tier is denied get a red dot: rows whose built-in default is denied were previously indistinguishable from ask rows; purely decorative, the semantics are carried by the text
  const gateDot = e.effective === "denied" ? `<span class="cap-dot" data-state="denied" aria-hidden="true"></span>` : "";
  const def = `<span class="cap-def" data-state="${esc(e.effective)}">${esc(decisionLabel(e.builtinDefault))}</span>`;
  return `<div class="setting-row cap-row" role="listitem" data-override="${current}"><div class="setting-info"><span class="setting-label cap-name">${gateDot}${esc(e.permission)}${badge}</span>${chipsRow}<span class="setting-hint">${esc(t("corePermBuiltinDefault"))}: ${def}</span></div><div class="perm-controls">${sel}</div></div>`;
}

/// Category-level bulk tier: a native select that reads the unified tier of that category's rows and writes back the selected state.
/// When rows have inconsistent tiers, show a disabled 'Mixed' placeholder — it is a status hint, not something selectable as an action.
function capBulkGroup(key: string, titleKey: I18nKey, rows: CorePermPolicy[]): string {
  const vals = new Set(rows.map((r) => r.override ?? ""));
  const mixed = vals.size > 1;
  const uniform = mixed ? "__mixed__" : ([...vals][0] ?? "");
  const opts: string[] = [];
  if (mixed) opts.push(`<option value="__mixed__" disabled selected>${esc(t("corePermMixed"))}</option>`);
  const opt = (val: string, labelKey: I18nKey): string =>
    `<option value="${esc(val)}"${!mixed && val === uniform ? " selected" : ""}>${esc(t(labelKey))}</option>`;
  opts.push(opt("", "corePermFollowDefault"));
  opts.push(opt("granted", "permGranted"));
  opts.push(opt("ask", "permAsk"));
  opts.push(opt("denied", "permDenied"));
  return `<div class="cap-bulk"><span class="setting-hint">${esc(t("corePermBulk"))}</span><select data-cap-bulk="${esc(key)}" data-cur="${esc(uniform)}" aria-label="${esc(t(titleKey))} · ${esc(t("corePermBulk"))}">${opts.join("")}</select></div>`;
}

/// Category card: one header row (category name + count + bulk tier), with the nested permission surface below (concentric radius 12 - 4 = 8).
/// The category name uses h3 + section aria-labelledby to enter the accessibility tree; all styling is still handled by .cap-cat-name.
function capCategory(g: { key: string; titleKey: I18nKey; rows: CorePermPolicy[] }): string {
  const headId = `capcat-${esc(g.key)}`;
  return `<section class="cap-cat" aria-labelledby="${headId}"><header class="cap-cat-head"><h3 class="cap-cat-name" id="${headId}">${esc(t(g.titleKey))}</h3><span class="cap-cat-count" aria-hidden="true">${g.rows.length}</span>${capBulkGroup(g.key, g.titleKey, g.rows)}</header><div class="cap-perms" role="list">${g.rows.map(corePermRow).join("")}</div></section>`;
}

/// Whether the last override write failed: after refresh the msg area shows 'Save failed' in red, cleared on consumption.
let corePermSaveFailed = false;

/// System Capabilities tab (docs/permissions.md 'Global policy (hard gate)'): grouped by category,
/// control granularity = permission (matching check_agent's enforcement granularity; capabilities that share a permission are listed together,
/// so subscription-type file.watch / screen.watch cannot be toggled independently of the base permission).
/// focusSel: the select selector whose focus is restored after redraw; passed only on the keyboard/change path, not on initial load or manual refresh.
async function refreshCorePerms(focusSel: string | null = null, allowEnter = false): Promise<void> {
  const box = document.getElementById("core-perm-list");
  if (!box) return;
  // The enter animation is allowed only on the 'tab-switch render'. .fresh-tab lingers in #tab-body across refreshes,
  // and this function rebuilds the cards every time — without explicitly turning it off, clicking a tier replays the blur-and-rise across the whole page,
  // and the row just changed disappears for over a second (measured: at 120ms later cards already have opacity 0 and filter blur(4px)).
  box.classList.toggle("cap-no-enter", !allowEnter);
  // Only on first load (container still empty) lay out the skeleton + aria-busy; later refreshes don't touch the DOM, per-row toggles don't flash the skeleton
  if (box.childElementCount === 0) {
    box.setAttribute("aria-busy", "true");
    box.innerHTML = listSkeleton(3);
  }
  let list: CorePermPolicy[] = [];
  let failed = false;
  try {
    list = await invoke<CorePermPolicy[]>("core_permission_list");
  } catch {
    failed = true;
  }
  box.removeAttribute("aria-busy");
  const msg = document.getElementById("core-perm-msg");
  if (failed) {
    if (msg) {
      msg.textContent = "";
      msg.classList.remove("cap-msg-failed");
    }
    // Fetch failure != no permissions: show a retryable error block, don't pass empty-state text off as it
    box.innerHTML = listError("corePermLoadFailed", "core-perm-retry");
    document.getElementById("core-perm-retry")?.addEventListener("click", () => void refreshCorePerms());
    return;
  }
  if (msg) {
    // If the last write failed, show 'Save failed' in red first; the next successful refresh restores the count
    msg.textContent = corePermSaveFailed ? t("corePermSaveFailed") : `${list.filter((e) => e.override !== null).length}/${list.length}`;
    msg.classList.toggle("cap-msg-failed", corePermSaveFailed);
    corePermSaveFailed = false;
  }
  if (list.length === 0) {
    box.innerHTML = `<p class="empty">${esc(t("corePermNone"))}</p>`;
    return;
  }
  const byPerm = new Map<string, CorePermPolicy>();
  list.forEach((e) => byPerm.set(e.permission, e));
  const groups: Array<{ key: string; titleKey: I18nKey; rows: CorePermPolicy[] }> = [];
  const claimed = new Set<string>();
  for (const cat of CORE_PERM_CATEGORIES) {
    const rows = cat.permissions
      .map((p) => byPerm.get(p))
      .filter((e): e is CorePermPolicy => !!e);
    if (rows.length === 0) continue;
    rows.forEach((e) => claimed.add(e.permission));
    groups.push({ key: cat.key, titleKey: cat.titleKey, rows });
  }
  const rest = list.filter((e) => !claimed.has(e.permission));
  if (rest.length > 0) groups.push({ key: "other", titleKey: "capCatOther", rows: rest });
  box.innerHTML = groups.map(capCategory).join("");
  // Persist a single permission: '' = clear override (follow default), otherwise write the override tier; failures set a flag, shown in the msg area after refresh
  const applyPerm = async (permission: string, value: string): Promise<void> => {
    try {
      if (value === "") {
        await invoke("core_permission_reset", { permission });
      } else {
        await invoke("core_permission_set", { permission, decision: value });
      }
    } catch {
      /* High-risk rejected granted etc.: the action must not be silent */
      corePermSaveFailed = true;
    }
  };
  box.querySelectorAll<HTMLSelectElement>("select[data-cap-bulk]").forEach((s) => {
    s.addEventListener("change", async () => {
      const key = s.dataset.capBulk ?? "";
      const value = s.value;
      if (value === "__mixed__") return;
      const rows = groups.find((g) => g.key === key)?.rows ?? [];
      for (const row of rows) {
        if (row.highRisk && value === "granted") continue;
        await applyPerm(row.permission, value);
      }
      // After redraw return focus to that category's bulk control: the native select fires change on arrow keys, and losing focus breaks the keyboard flow
      await refreshCorePerms(`select[data-cap-bulk="${key}"]`);
    });
  });
  box.querySelectorAll<HTMLSelectElement>("select[data-cap-perm]").forEach((s) => {
    s.addEventListener("change", async () => {
      const permission = s.dataset.capPerm ?? "";
      await applyPerm(permission, s.value);
      await refreshCorePerms(`select[data-cap-perm="${permission}"]`);
    });
  });
  if (focusSel) box.querySelector<HTMLSelectElement>(focusSel)?.focus();
}
