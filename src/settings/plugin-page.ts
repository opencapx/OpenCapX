import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { getLocale, t } from "../i18n";
import {
  cssEscape,
  esc,
  escAttr,
  getPluginNameById,
  getPluginPageReturn,
  getTab,
  refreshPluginConfig,
  switchTab,
} from "./shared";
import type {
  Cond,
  LocalizedText,
  PluginSettingsView,
  SettingDecl,
  ValidateRule,
} from "./types";

export function renderPluginPageTab(body: HTMLElement): void {
  // Plugin settings page: declared settings[] -> form; not declared -> config editor.
  // Back appears only when entered from the plugin detail card — the sidebar section is itself the entry point.
  // The hero establishes identity first (letter avatar + name + id + form/JSON badges); the body fills in when it arrives asynchronously.
  const id = getTab().slice("plugin:".length);
  const name = getPluginNameById().get(id) ?? id;
  const monogram = (name.trim().charAt(0) || id.charAt(0) || "?").toUpperCase();
  const back = getPluginPageReturn()
    ? `<button class="btn ghost plugin-detail-back plug-back" data-plugin-page-back type="button">‹ ${esc(t("pluginDetailBack"))}</button>`
    : "";
  body.innerHTML = `<div class="plugin-page-wrap">
    <div class="plug-hero">
      ${back}
      <div class="plug-hero-main">
        <span class="plug-avatar" aria-hidden="true">${esc(monogram)}</span>
        <div class="plug-hero-info">
          <span class="plug-hero-name">${esc(name)}</span>
          <span class="plug-hero-id">${esc(id)}</span>
        </div>
        <span class="plug-hero-badge"></span>
      </div>
    </div>
    <div class="plugin-page-list" id="plugin-page-body"></div>
  </div>`;
  body.querySelector("[data-plugin-page-back]")?.addEventListener("click", () => {
    const to = getPluginPageReturn();
    if (to) switchTab(to); // switchTab clears pluginPageReturn itself
  });
  void renderPluginPageSettings(id);
}

/// Plugin settings page body (only call site: render's plugin: branch).
/// Declared settings[] -> declarative form; not declared -> KV/JSON dual-mode config editor (set_plugin_config).
/// If, by the time it returns asynchronously, the page has switched away / the node was replaced by a re-render -> give up, don't write.
async function renderPluginPageSettings(id: string): Promise<void> {
  const host = document.getElementById("plugin-page-body");
  if (!host) return;
  const live = (): boolean =>
    getTab() === `plugin:${id}` && document.getElementById("plugin-page-body") === host;
  // The hero badges are in the hero (outside host): declared count / JSON mode, filled only when data arrives
  const setBadge = (text: string): void => {
    const badge = host.closest(".plugin-page-wrap")?.querySelector(".plug-hero-badge");
    if (badge) badge.textContent = text;
  };
  let view: PluginSettingsView | null = null;
  try {
    const v = await invoke<PluginSettingsView>("list_plugin_settings", { id });
    if (v && Array.isArray(v.settings) && v.settings.length > 0) view = v;
  } catch {
    /* No declaration / command unavailable: use the config editor */
  }
  if (!live()) return;
  if (view) {
    // The form is the only write entry; raw values are kept in a read-only collapsible (so the whole object the plugin actually reads stays visible)
    setBadge(t("pluginCfgDeclaredBadge").replace("{n}", String(view.settings.length)));
    const raw = JSON.stringify(view.values ?? {}, null, 2);
    host.innerHTML = `<div class="plugin-page-body-inner">${renderDeclaredSettings(id, view)}
      <div class="plug-sect plug-raw"><details class="cfg-raw-details"><summary class="setting-hint">${esc(t("pluginConfigRawView"))}</summary><textarea class="cfg-textarea" data-page-cfg-raw readonly spellcheck="false" rows="6">${esc(raw)}</textarea></details></div></div>`;
    wireDeclaredSettings(host);
    return;
  }
  let cfg: Record<string, unknown> = {};
  try {
    cfg = (await invoke<Record<string, unknown>>("get_plugin_config", { id })) ?? {};
  } catch {
    /* Empty config */
  }
  if (!live()) return;
  setBadge(t("pluginCfgJsonBadge"));
  // No declared settings[]: per-key KV CRUD (default) + an advanced JSON mode.
  // paintEditor fully repaints host and the button rows and rewires every time — the buttons are all new nodes,
  // listeners don't accumulate, and no clone-guard is needed.
  // Clearing the whole object (Reset) is only in JSON mode: that is the 'I know what I'm doing' advanced surface, and it has
  // a confirm fallback — the per-key deletion in KV mode already covers everyday edits.
  const actionsHtml = (mode: CfgMode): string =>
    `<div class="cfg-actions"><button class="btn ghost" data-page-cfg-mode type="button">${esc(mode === "kv" ? t("cfgModeJson") : t("cfgModeKv"))}</button>${mode === "json" ? `<button class="btn ghost danger" data-page-cfg-reset type="button">${esc(t("pluginConfigReset"))}</button>` : ""}<button class="btn" data-page-cfg-save type="button">${esc(t("pluginConfigSave"))}</button><span class="setting-hint" data-page-cfg-msg></span></div>`;
  const paintEditor = (mode: CfgMode, current: Record<string, unknown>): void => {
    host.innerHTML = `<div class="plugin-page-body-inner">${renderPageCfgBody(current, mode)}${actionsHtml(mode)}</div>`;
    wirePageCfgEditor(id, host, mode);
    host.querySelector("button[data-page-cfg-mode]")?.addEventListener("click", async () => {
      let latest: Record<string, unknown> = current; // switching modes uses the last persisted value, dropping unsaved edits
      try {
        latest = (await invoke<Record<string, unknown>>("get_plugin_config", { id })) ?? {};
      } catch {
        /* Keep current */
      }
      if (!live()) return; // switched away during the async period
      paintEditor(mode === "kv" ? "json" : "kv", latest);
    });
    // Clear config (visible in JSON mode only): after confirm, delete_plugin_config and repaint back to an empty KV object
    host.querySelector("button[data-page-cfg-reset]")?.addEventListener("click", async () => {
      if (!window.confirm(t("pluginConfigResetConfirm"))) return;
      try {
        await invoke("delete_plugin_config", { id });
      } catch (err) {
        const m = host.querySelector<HTMLElement>("[data-page-cfg-msg]");
        if (m) {
          m.textContent = `${t("pluginConfigSaveFailed")}: ${(err as Error).message ?? err}`;
          m.classList.add("cfg-err");
        }
        return;
      }
      paintEditor("kv", {});
      const m = host.querySelector<HTMLElement>("[data-page-cfg-msg]");
      if (m) m.textContent = t("pluginConfigResetDone");
    });
  };
  paintEditor("kv", cfg);
}

/// One KV editor row: key is read-only (renaming = delete and re-add, avoiding accidental damage to keys the plugin reads); value is editable.
/// Every row stores the original JSON in data-orig-json — if the value was untouched it is written back as-is, without a text->scalar round trip
/// (otherwise a string-shaped value like '1234' would be stored as a number). Non-scalars (object/array/null) are read-only;
/// to change the structure, switch to JSON mode.
function kvRowHtml(key: string, value: unknown): string {
  const complex = typeof value === "object"; // null is also a non-scalar: read-only + write back the original JSON
  const shown = escAttr(complex ? JSON.stringify(value) : String(value));
  // hover can only report the facts it has (key + value type): this plugin declares no schema, so the host knows no more semantics
  const typeName = value === null ? "null" : Array.isArray(value) ? "array" : typeof value;
  return `<div class="kv-row" title="${escAttr(`${key}: ${typeName}`)}"><input class="kv-key" value="${escAttr(key)}" readonly/><input class="kv-val" value="${shown}"${complex ? " readonly" : ""} data-orig-json="${escAttr(JSON.stringify(value))}"/><button class="btn ghost danger" data-kv-del type="button" title="${escAttr(t("remove"))}" aria-label="${escAttr(t("remove"))}">×</button></div>`;
}

/// Input string -> scalar: true/false/numbers stored by type; quoted -> forced string; everything else a plain string.
function coerceScalar(raw: string): unknown {
  const s = raw.trim();
  if (s === "true") return true;
  if (s === "false") return false;
  if (s.length >= 2 && s.startsWith('"') && s.endsWith('"')) return s.slice(1, -1);
  if (/^-?\d+(\.\d+)?$/.test(s)) {
    const n = Number(s);
    if (String(n) === s) return n;
  }
  return raw;
}

/// KV editor body: a row per existing key + an 'Add key' row. The whole block sits in a .plug-sect card, using the same depth language as declarative settings.
function renderKVEditor(cfg: Record<string, unknown>): string {
  const rows = Object.entries(cfg).map(([k, v]) => kvRowHtml(k, v)).join("");
  return `<div class="plug-sect plug-kv-sect"><p class="kv-undeclared-hint">${esc(t("pluginCfgUndeclaredHint"))}</p><div class="kv-editor">${rows}
    <div class="kv-row kv-add"><input class="kv-key" data-kv-new-key type="text" placeholder="${escAttr(t("cfgKVKeyPlaceholder"))}"/><input class="kv-val" data-kv-new-val type="text" placeholder="${escAttr(t("cfgKVValuePlaceholder"))}"/><button class="btn ghost" data-kv-add type="button" title="${escAttr(t("cfgKVAdd"))}" aria-label="${escAttr(t("cfgKVAdd"))}">+</button></div></div></div>`;
}

type CfgMode = "kv" | "json";

/// Config editor body for plugins without declarations. KV mode by default (per-key CRUD);
/// 'Advanced: JSON' switches to a whole-object textarea (fallback for nested structures); both modes use the same persist command.
function renderPageCfgBody(cfg: Record<string, unknown>, mode: CfgMode): string {
  if (mode === "json") {
    return `<div class="plug-sect plug-json-sect"><textarea class="cfg-textarea" data-page-cfg-textarea spellcheck="false" rows="6">${esc(JSON.stringify(cfg, null, 2))}</textarea></div>`;
  }
  return renderKVEditor(cfg);
}

/// host = #plugin-page-body (fully repainted on every paintEditor; buttons are all new nodes, so listeners don't accumulate).
function wirePageCfgEditor(id: string, host: HTMLElement, mode: CfgMode): void {
  const msg = host.querySelector<HTMLElement>("[data-page-cfg-msg]");
  if (!msg) return;
  const fail = (text: string): void => {
    msg.textContent = text;
    msg.classList.add("cfg-err");
  };
  const clearMsg = (): void => {
    msg.textContent = "";
    msg.classList.remove("cfg-err");
  };
  if (mode === "json") {
    // JSON mode: the original textarea semantics (parse validation + set_plugin_config)
    host.querySelector("button[data-page-cfg-save]")?.addEventListener("click", async () => {
      const ta = host.querySelector<HTMLTextAreaElement>("textarea[data-page-cfg-textarea]");
      if (!ta) return;
      let parsed: unknown;
      try {
        parsed = JSON.parse(ta.value);
      } catch (err) {
        fail(`${t("pluginConfigInvalid")}: ${(err as Error).message}`);
        return;
      }
      if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
        fail(t("pluginConfigInvalid"));
        return;
      }
      try {
        await invoke("set_plugin_config", { id, value: parsed });
        msg.textContent = t("pluginConfigSaved");
        msg.classList.remove("cfg-err");
      } catch (err) {
        fail(`${t("pluginConfigSaveFailed")}: ${(err as Error).message ?? err}`);
      }
    });
    return;
  }
  // KV mode: read rows -> object; key validation + dedupe; rows whose value was untouched (the value still equals the painted
  // defaultValue) are written back to data-orig-json as-is; only rows the user changed and newly added rows go through coerceScalar's
  // text->scalar rules.
  // out has no prototype: a key can be named __proto__ (whatever a plugin writes via core::config::set is what it is),
  // on a plain object literal out["__proto__"] = v only changes the prototype and the key is silently dropped.
  const readObject = (): Record<string, unknown> | null => {
    const out = Object.create(null) as Record<string, unknown>;
    for (const row of host.querySelectorAll<HTMLDivElement>(".kv-row:not(.kv-add)")) {
      const keyEl = row.querySelector<HTMLInputElement>(".kv-key");
      const valEl = row.querySelector<HTMLInputElement>(".kv-val");
      if (!keyEl || !valEl) continue;
      const key = keyEl.readOnly ? keyEl.value : keyEl.value.trim(); // keys stored in the library are carried over as-is (leading/trailing spaces are part of the key)
      // Validate only keys the user can change: keys stored in the library aren't restricted by character set (written by plugins via core::config::set),
      // read-only rows are written back as-is, otherwise the editor gets stuck on an unfixable error (renaming = delete and re-add).
      if (!keyEl.readOnly && !/^[A-Za-z_][A-Za-z0-9_.-]{0,63}$/.test(key)) {
        fail(`${t("cfgKVBadKey")}: ${key || t("cfgKVEmptyKey")}`);
        return null;
      }
      if (Object.prototype.hasOwnProperty.call(out, key)) {
        fail(`${t("cfgKVDupKey")}: ${key}`);
        return null;
      }
      out[key] = valEl.dataset.origJson !== undefined && !valEl.classList.contains("kv-touched") ? JSON.parse(valEl.dataset.origJson) : coerceScalar(valEl.value);
    }
    return out;
  };
  // Delete row
  host.querySelectorAll("button[data-kv-del]").forEach((b) => {
    b.addEventListener("click", () => {
      b.closest(".kv-row")?.remove();
      clearMsg();
    });
  });
  // Value rows the user touched are stored by their typed form (retyping the same text = explicitly changing the type); untouched rows are written back as-is,
  // using data-orig-json to preserve the difference between the string '1234' and the number 1234.
  host.querySelectorAll<HTMLInputElement>(".kv-val").forEach((v) => {
    v.addEventListener("input", () => v.classList.add("kv-touched"));
  });
  // Add row: validate key and value together; once valid it enters the DOM (the key is still editable), with full validation only on save
  const addRow = (): void => {
    const keyInput = host.querySelector<HTMLInputElement>("[data-kv-new-key]");
    const valInput = host.querySelector<HTMLInputElement>("[data-kv-new-val]");
    if (!keyInput || !valInput) return;
    const key = keyInput.value.trim();
    if (!/^[A-Za-z_][A-Za-z0-9_.-]{0,63}$/.test(key)) {
      fail(`${t("cfgKVBadKey")}: ${key || t("cfgKVEmptyKey")}`);
      return;
    }
    const rowHtml = `<div class="kv-row"><input class="kv-key" value="${escAttr(key)}"/><input class="kv-val" value="${escAttr(valInput.value)}"/><button class="btn ghost danger" data-kv-del type="button" title="${escAttr(t("remove"))}" aria-label="${escAttr(t("remove"))}">×</button></div>`;
    keyInput.value = "";
    valInput.value = "";
    // Insert before the 'Add key' row: otherwise the add row is no longer last and the editor reads [key][add row][new key].
    const editor = host.querySelector(".kv-editor");
    const addRowEl = editor?.querySelector(".kv-add");
    addRowEl?.insertAdjacentHTML("beforebegin", rowHtml);
    const added = addRowEl?.previousElementSibling as HTMLElement | null | undefined;
    added?.querySelector("button[data-kv-del]")?.addEventListener("click", () => {
      added.remove();
      clearMsg();
    });
    clearMsg();
  };
  host.querySelector("button[data-kv-add]")?.addEventListener("click", addRow);
  host.querySelector<HTMLInputElement>("[data-kv-new-val]")?.addEventListener("keydown", (e) => {
    if (e.key === "Enter") addRow();
  });
  // Save: content already typed in the 'Add key' row is committed first, otherwise the just-typed key/value is silently dropped while still reporting Saved
  host.querySelector("button[data-page-cfg-save]")?.addEventListener("click", async () => {
    const pending = host.querySelector<HTMLInputElement>("[data-kv-new-key]");
    if (pending && pending.value.trim() !== "") {
      addRow();
      if (pending.value.trim() !== "") return; // addRow rejected it (invalid key): the error is already shown
    }
    const obj = readObject();
    if (!obj) return;
    try {
      await invoke("set_plugin_config", { id, value: obj });
      msg.textContent = t("pluginConfigSaved");
      msg.classList.remove("cfg-err");
    } catch (err) {
      fail(`${t("pluginConfigSaveFailed")}: ${(err as Error).message ?? err}`);
    }
  });
}

/// Localization resolution for plugin text (frozen order, same as the Rust side): current language -> en -> lexicographically smallest key.
/// Do not write fallback logic elsewhere — both sides must use exactly the same order.
function resolveText(v: LocalizedText | undefined, locale: string): string {
  if (v === undefined) return "";
  if (typeof v === "string") return v;
  const direct = v[locale];
  if (direct !== undefined) return direct;
  const en = v["en"];
  if (en !== undefined) return en;
  const keys = Object.keys(v).sort();
  return keys.length > 0 ? v[keys[0]] : "";
}

/// section grouping key: compare raw declaration values (string or mapping), not resolved text.
/// Mappings are sorted by key and joined, so the same section isn't split into two headings because of key order.
function sectionKey(section: LocalizedText | undefined): string {
  if (section === undefined) return "";
  if (typeof section === "string") return `s:${section}`;
  return `m:${Object.keys(section)
    .sort()
    .map((k) => `${k}=${section[k]}`)
    .join("\u0001")}`;
}

/// Predicate evaluation (pure function, no DOM). secret values never enter values, so isSet must also check secretsSet;
/// unknown ops always return false — Rust rejects them at install time; this is only defensive and never throws.
function evalCond(cond: Cond, values: Record<string, unknown>, secretsSet: readonly string[]): boolean {
  switch (cond.op) {
    case "equals":
      return values[cond.key] === cond.value;
    case "notEquals":
      return values[cond.key] !== cond.value;
    case "in":
      return cond.values.includes(values[cond.key]);
    case "isSet":
      return secretsSet.includes(cond.key) || values[cond.key] !== undefined;
    case "all":
      return cond.conds.every((c) => evalCond(c, values, secretsSet));
    case "any":
      return cond.conds.some((c) => evalCond(c, values, secretsSet));
    case "not":
      return !evalCond(cond.cond, values, secretsSet);
    default:
      return false;
  }
}

/// Pass determination for a single rule. 'Not set' (missing / null / '' / empty array) is handled by required alone:
/// all other rules are skipped — an optional field left empty = use the plugin default, and must not be permanently failed by pattern/minLength with no way to clear it.
/// Rules whose types don't match always pass, so a bad manifest doesn't lock the whole form (already validated by Rust at install time).
function rulePasses(r: ValidateRule, value: unknown): boolean {
  const unset = value === undefined || value === null || value === "" || (Array.isArray(value) && value.length === 0);
  if (r.type !== "required" && unset) return true;
  switch (r.type) {
    case "required":
      return !(value === undefined || value === null || value === "" || (Array.isArray(value) && value.length === 0));
    case "minLength":
      return typeof value !== "string" || value.length >= r.value;
    case "maxLength":
      return typeof value !== "string" || value.length <= r.value;
    case "min":
    case "max": {
      if (value === "" || value === undefined || value === null) return true;
      const n = typeof value === "number" ? value : Number(value);
      if (!Number.isFinite(n)) return true;
      return r.type === "min" ? n >= r.value : n <= r.value;
    }
    case "pattern": {
      if (typeof value !== "string") return true;
      try {
        return new RegExp(r.regex).test(value);
      } catch {
        return true;
      }
    }
    default:
      return true;
  }
}

/// Default text (not reached when the rule carries its own message).
function defaultRuleMessage(r: ValidateRule): string {
  switch (r.type) {
    case "required":
      return t("pluginValidateRequired");
    case "minLength":
      return t("pluginValidateMinLength").replace("{n}", String(r.value));
    case "maxLength":
      return t("pluginValidateMaxLength").replace("{n}", String(r.value));
    case "min":
      return t("pluginValidateMin").replace("{n}", String(r.value));
    case "max":
      return t("pluginValidateMax").replace("{n}", String(r.value));
    case "pattern":
      return t("pluginValidatePattern");
    default:
      return t("pluginConfigInvalid");
  }
}

/// Rule evaluation: return the first failing message, or null if all pass. A rule's own message takes priority.
function evalRules(rules: ValidateRule[], value: unknown): string | null {
  for (const r of rules) {
    if (rulePasses(r, value)) continue;
    // message may be a localized mapping: resolved in resolveText's frozen order (shared by the mount check and the pre-write gate)
    return resolveText(r.message, getLocale()) || defaultRuleMessage(r);
  }
  return null;
}

/// Declaration table registered during render: pair = '<pluginId>|<key>' -> SettingDecl.
/// Before writing, look up validation rules by pair (re-renders overwrite entries, and stale entries are never referenced by the DOM).
const declaredDecls = new Map<string, SettingDecl>();

/** Control field encoding: '<pluginId>|<settingKey>' (neither id nor key contains '|') */
function splitSettingPair(pair: string): [string, string] {
  const i = pair.indexOf("|");
  return [pair.slice(0, i), pair.slice(i + 1)];
}

function renderDeclaredSettings(id: string, view: PluginSettingsView): string {
  // Group by section: one .plug-sect card per segment, with small cards inside (separated by gap, no border-bottom);
  // declarations without a section go into one untitled card (the heading appears only when section !== undefined, matching existing behavior).
  const sects: { title: string | null; rows: string[] }[] = [];
  let current: { title: string | null; rows: string[] } | null = null;
  let lastSection: string | null = null;
  // order stable sort: declared orders with smaller values first; equal and undeclared orders fall back to manifest order
  const ordered = view.settings
    .map((s, i) => ({ s, i }))
    .sort((a, b) => (a.s.order ?? Number.MAX_SAFE_INTEGER) - (b.s.order ?? Number.MAX_SAFE_INTEGER) || a.i - b.i)
    .map((x) => x.s);
  for (const s of ordered) {
    declaredDecls.set(`${id}|${s.key}`, s);
    // visible predicate is false: the whole row is not rendered (the predicate depends only on stored values and recomputes immediately after a write)
    if (s.visible && !evalCond(s.visible, view.values, view.secretsSet)) continue;
    const key = sectionKey(s.section);
    if (key !== lastSection || current === null) {
      // Consecutive declarations with the same section share a heading (grouped by raw value); the heading takes the resolved text of the segment's first item
      current = { title: s.section !== undefined ? esc(resolveText(s.section, getLocale())) : null, rows: [] };
      sects.push(current);
      lastSection = key;
    }
    current.rows.push(renderDeclaredRow(id, s, view));
  }
  const cards = sects
    .map((sec) => {
      const title = sec.title !== null ? `<p class="settings-group-title">${sec.title}</p>` : "";
      return `<div class="plug-sect">${title}${sec.rows.join("")}</div>`;
    })
    .join("");
  return `<div class="declared-settings"><p class="setting-hint">${esc(t("pluginSettingDeclared"))}</p>${cards}</div>`;
}

/// Single declaration -> control. When the disabled predicate is true the control is disabled and the whole row is dimmed;
/// on mount, run the rules once against stored/default values to flag bad values left by older plugin versions directly (hint only, non-blocking).
function renderDeclaredRow(id: string, s: SettingDecl, view: PluginSettingsView): string {
  const pair = `${id}|${s.key}`;
  const locale = getLocale();
  const label = esc(resolveText(s.label, locale) || s.key);
  const descText = resolveText(s.description, locale);
  // Deprecated: the control stays usable; the row carries a marker + reason (text is localized; an empty text is caught by Rust validation, with a generic fallback word here).
  // The badge shares the row with the label (inline), no longer pushed below the description by the flex column.
  const depText = resolveText(s.deprecated, locale) || (s.deprecated ? t("pluginSettingDeprecated") : "");
  const depBadge = s.deprecated ? `<span class="cfg-deprecated-badge">${esc(t("pluginSettingDeprecated"))}</span>` : "";
  const labelLine = `<span class="setting-label-line"><span class="setting-label">${label}</span>${depBadge}</span>`;
  const desc = (descText ? `<span class="setting-hint">${esc(descText)}</span>` : "")
    + (s.deprecated ? `<span class="setting-hint">${esc(depText)}</span>` : "");
  // Search haystack (all lowercase): resolved label/description + the raw values of every language in the mapping
  // + aliases + key + plugin id. Chinese queries match even in an English UI (thanks to the raw values in the mapping).
  const parts: string[] = [resolveText(s.label, locale), descText, s.key, id, ...(s.aliases ?? [])];
  if (typeof s.label === "object") parts.push(...Object.values(s.label));
  if (typeof s.description === "object") parts.push(...Object.values(s.description));
  for (const o of s.options ?? []) {
    if (typeof o !== "string" && o.label) parts.push(resolveText(o.label, locale) ?? "", ...Object.values(o.label));
  }
  const haystack = esc(parts.join("\n").toLowerCase()).replace(/"/g, "&quot;");
  const off = !!s.disabled && evalCond(s.disabled, view.values, view.secretsSet);
  const dis = off ? " disabled" : "";
  const cls = off ? "setting-row plug-row cfg-row-off" : "setting-row plug-row";
  const rowOpen = `<div class="${cls}" data-cfg-search="${haystack}">`;
  const rowOpenV = `<div class="setting-row vertical plug-row${off ? " cfg-row-off" : ""}" data-cfg-search="${haystack}">`;
  // secret values live in the keychain and the frontend can't get them — don't evaluate at mount (otherwise required would always false-positive)
  const stored = view.values[s.key] ?? s.default;
  const mountFail = s.type === "secret" ? null : evalRules(s.validate ?? [], stored);
  const msg = `<span class="setting-hint${mountFail ? " cfg-err" : ""}" data-set-msg="${esc(pair)}">${mountFail ? esc(mountFail) : ""}</span>`;
  const save = `<button class="btn" data-set-save="${esc(pair)}" data-set-kind="${esc(s.type)}" type="button"${dis}>${esc(t("pluginConfigSave"))}</button>`;
  // Modified marker + per-key restore default: value present and != default -> marker;
  // the restore button appears only when a default exists (keys without a default have no 'default value' to write back, only the marker).
  // secret/button/list never enter settings_view.values -> hasVal is always false, so the marker naturally never appears.
  const hasVal = view.values[s.key] !== undefined;
  const modified = hasVal && JSON.stringify(view.values[s.key]) !== JSON.stringify(s.default);
  const mod = modified ? `<span class="cfg-mod" title="${esc(t("pluginSettingModified"))}">●</span>` : "";
  const rst = modified && s.default !== undefined
    ? `<button class="btn ghost" data-set-reset-default="${esc(pair)}" type="button"${dis} title="${esc(t("pluginSettingResetDefault"))}">${esc(t("pluginSettingResetDefault"))}</button>`
    : "";
  switch (s.type) {
    case "toggle": {
      const checked = view.values[s.key] === true ? " checked" : "";
      return `${rowOpen}<div class="setting-info">${labelLine}${desc}</div><input type="checkbox" data-set-toggle="${esc(pair)}"${checked}${dis}/>${msg}${mod}${rst}</div>`;
    }
    case "dropdown": {
      const cur = String(view.values[s.key] ?? "");
      const opts = (s.options ?? [])
        .map((o) => {
          const value = typeof o === "string" ? o : o.value;
          const text = typeof o === "string" ? o : (resolveText(o.label, locale) || o.value);
          return `<option value="${esc(value)}"${value === cur ? " selected" : ""}>${esc(text)}</option>`;
        })
        .join("");
      return `${rowOpen}<div class="setting-info">${labelLine}${desc}</div><select data-set-select="${esc(pair)}"${dis}>${opts}</select>${msg}${mod}${rst}</div>`;
    }
    case "radio-group": {
      const cur = String(view.values[s.key] ?? "");
      // Reuse the data-set-select hook: it's the same 'change -> store the option string', so don't open a second save path for it
      const radios = (s.options ?? [])
        .map((o) => {
          const value = typeof o === "string" ? o : o.value;
          const text = typeof o === "string" ? o : (resolveText(o.label, locale) || o.value);
          return `<label class="cfg-radio"><input type="radio" name="${esc(pair)}" value="${esc(value)}" data-set-select="${esc(pair)}"${value === cur ? " checked" : ""}${dis}/><span>${esc(text)}</span></label>`;
        })
        .join("");
      return `${rowOpen}<div class="setting-info">${labelLine}${desc}</div><div class="cfg-radio-group">${radios}</div>${msg}${mod}${rst}</div>`;
    }
    case "color": {
      const val = typeof view.values[s.key] === "string" ? String(view.values[s.key]) : "#000000";
      return `${rowOpen}<div class="setting-info">${labelLine}${desc}</div><input type="color" data-set-input="${esc(pair)}" value="${esc(val)}"${dis}/>${save}${msg}${mod}${rst}</div>`;
    }
    case "slider": {
      const min = s.min ?? 0;
      const max = s.max ?? 100;
      const step = s.step ?? 1;
      const v = view.values[s.key];
      const val = typeof v === "number" && Number.isFinite(v) ? v : min;
      // The readout updates live while dragging (without persisting); persisting uses the same save button, and validation takes the same path
      return `${rowOpen}<div class="setting-info">${labelLine}${desc}</div><div class="cfg-slider"><input type="range" min="${esc(String(min))}" max="${esc(String(max))}" step="${esc(String(step))}" data-set-input="${esc(pair)}" value="${esc(String(val))}"${dis}/><span class="setting-hint" data-set-readout="${esc(pair)}">${esc(String(val))}</span></div>${save}${msg}${mod}${rst}</div>`;
    }
    case "secret": {
      const set = view.secretsSet.includes(s.key);
      const hint = set ? t("pluginSettingSecretSet") : t("pluginSettingSecretUnset");
      return `${rowOpen}<div class="setting-info">${labelLine}${desc}<span class="setting-hint" data-set-mask="${esc(pair)}">${esc(hint)}</span></div><input type="password" data-set-input="${esc(pair)}" placeholder="********" spellcheck="false"${dis}/>${save}${msg}${mod}${rst}</div>`;
    }
    case "button": {
      return `${rowOpen}<div class="setting-info">${labelLine}${desc}</div><button class="btn ghost" data-set-action="${esc(pair)}" type="button"${dis}>${label}</button>${msg}${mod}${rst}</div>`;
    }
    case "textarea": {
      const val = typeof view.values[s.key] === "string" ? String(view.values[s.key]) : "";
      return `${rowOpenV}<div class="setting-info">${labelLine}${desc}</div><textarea class="cfg-textarea" rows="3" spellcheck="false" data-set-input="${esc(pair)}"${dis}>${esc(val)}</textarea>${save}${msg}${mod}${rst}</div>`;
    }
    case "number": {
      const v = view.values[s.key];
      const val = typeof v === "number" ? String(v) : "";
      return `${rowOpen}<div class="setting-info">${labelLine}${desc}</div><input type="number" step="1" data-set-input="${esc(pair)}" value="${esc(val)}"${dis}/>${save}${msg}${mod}${rst}</div>`;
    }
    case "list": {
      // P3 — items belong to the plugin process: fetched at mount (op:list), row actions forwarded immediately;
      // no save button (each op makes the plugin persist and return a new array). msg still renders like other types:
      // an op failure writes an inline hint rather than firing an alert (the only modal in the settings window).
      return `${rowOpenV}<div class="setting-info">${labelLine}${desc}</div>
        <div class="kv-editor" data-list-body="${esc(pair)}"><span class="setting-hint">${esc(t("pluginSettingListLoading"))}</span></div>
        <div class="kv-row kv-add"><input class="kv-val" data-list-new type="text" placeholder="${esc(t("pluginSettingListAddPlaceholder"))}"${dis}/><button class="btn ghost" data-list-add="${esc(pair)}" type="button" title="${escAttr(t("pluginSettingListAdd"))}" aria-label="${escAttr(t("pluginSettingListAdd"))}"${dis}>+</button></div>${msg}${mod}${rst}</div>`;
    }
    default: {
      // text / path
      const val = typeof view.values[s.key] === "string" ? String(view.values[s.key]) : "";
      // pick takes effect only on path: file -> pick a file; default/other -> pick a directory (preserving existing behavior)
      const browse =
        s.type === "path"
          ? `<button class="btn ghost" data-set-browse="${esc(pair)}" data-set-pick="${esc(s.pick === "file" ? "file" : "directory")}" type="button"${dis}>${esc(t("pluginSettingBrowse"))}</button>`
          : "";
      return `${rowOpen}<div class="setting-info">${labelLine}${desc}</div><input type="text" spellcheck="false" data-set-input="${esc(pair)}" value="${esc(val)}"${dis}/>${browse}${save}${msg}${mod}${rst}</div>`;
    }
  }
}

/// Per-key restore default: write back the declaration's default — through the same saveDeclaredSetting
/// validation/hint/re-render pipeline, without opening another write path.
function wireResetDefault(box: HTMLElement, msgOf: (pair: string) => Element | null): void {
  box.querySelectorAll<HTMLButtonElement>("button[data-set-reset-default]").forEach((b) => {
    b.addEventListener("click", () => {
      const pair = b.dataset.setResetDefault ?? "";
      const [id, key] = splitSettingPair(pair);
      const decl = declaredDecls.get(pair);
      if (!decl || decl.default === undefined) return;
      void saveDeclaredSetting(id, key, decl.default, msgOf(pair));
    });
  });
}

async function saveDeclaredSetting(
  id: string,
  key: string,
  value: unknown,
  msg: Element | null
): Promise<boolean> {
  // Run validation first (rules looked up by pair): on failure hint in place, don't write or re-render — the value the user just typed must stay in the box
  const failure = evalRules(declaredDecls.get(`${id}|${key}`)?.validate ?? [], value);
  if (failure) {
    if (msg) {
      msg.textContent = failure;
      msg.classList.add("cfg-err");
    }
    return false;
  }
  try {
    await invoke("set_plugin_setting", { id, key, value });
    if (msg) {
      msg.textContent = t("pluginConfigSaved");
      msg.classList.remove("cfg-err");
    }
    // On success re-render with the new values: visible/disabled predicates recompute automatically, no manual update() needed
    await refreshPluginConfig();
    // The plugin settings page shares this data: the `if (getTab().startsWith("plugin:")) render()` inside refreshPluginConfig
    // re-renders it instead of calling the detail-page render separately
    return true;
  } catch (err) {
    if (msg) {
      msg.textContent = `${t("pluginConfigSaveFailed")}: ${(err as Error).message ?? err}`;
      msg.classList.add("cfg-err");
    }
    return false;
  }
}

function wireDeclaredSettings(box: HTMLElement): void {
  const msgOf = (pair: string): Element | null =>
    box.querySelector(`span[data-set-msg="${cssEscape(pair)}"]`);

  wireResetDefault(box, msgOf);

  box.querySelectorAll<HTMLInputElement>("input[data-set-toggle]").forEach((el) => {
    el.addEventListener("change", async () => {
      const pair = el.dataset.setToggle ?? "";
      const [id, key] = splitSettingPair(pair);
      await saveDeclaredSetting(id, key, el.checked, msgOf(pair));
    });
  });

  // dropdown and radio-group share this one 'change -> store the option string' path (radio's el.value is the selected option)
  box.querySelectorAll<HTMLSelectElement | HTMLInputElement>("select[data-set-select], input[data-set-select]").forEach((el) => {
    el.addEventListener("change", async () => {
      const pair = el.dataset.setSelect ?? "";
      const [id, key] = splitSettingPair(pair);
      await saveDeclaredSetting(id, key, el.value, msgOf(pair));
    });
  });

  // slider readout: shown live while dragging; persisting still goes through the save button
  box.querySelectorAll<HTMLInputElement>('input[type="range"][data-set-input]').forEach((el) => {
    el.addEventListener("input", () => {
      const pair = el.dataset.setInput ?? "";
      const out = box.querySelector(`span[data-set-readout="${cssEscape(pair)}"]`);
      if (out) out.textContent = el.value;
    });
  });

  box.querySelectorAll<HTMLButtonElement>("button[data-set-save]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const pair = btn.dataset.setSave ?? "";
      const kind = btn.dataset.setKind ?? "text";
      const [id, key] = splitSettingPair(pair);
      const input = box.querySelector<HTMLInputElement | HTMLTextAreaElement>(
        `[data-set-input="${cssEscape(pair)}"]`
      );
      if (!input) return;
      let value: unknown = input.value;
      if (kind === "number") {
        const n = Number.parseInt(input.value, 10);
        if (!Number.isFinite(n) || String(n) !== input.value.trim()) {
          const msg = msgOf(pair);
          if (msg) {
            msg.textContent = t("pluginConfigInvalid");
            msg.classList.add("cfg-err");
          }
          return;
        }
        value = n;
      }
      if (kind === "slider") {
        const n = Number(input.value);
        if (!Number.isFinite(n)) {
          const msg = msgOf(pair);
          if (msg) {
            msg.textContent = t("pluginConfigInvalid");
            msg.classList.add("cfg-err");
          }
          return;
        }
        value = n;
      }
      const ok = await saveDeclaredSetting(id, key, value, msgOf(pair));
      if (ok && kind === "secret") {
        input.value = "";
        const mask = box.querySelector(`span[data-set-mask="${cssEscape(pair)}"]`);
        if (mask) mask.textContent = t("pluginSettingSecretSet");
      }
    });
  });

  box.querySelectorAll<HTMLButtonElement>("button[data-set-browse]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const pair = btn.dataset.setBrowse ?? "";
      const input = box.querySelector<HTMLInputElement>(
        `[data-set-input="${cssEscape(pair)}"]`
      );
      if (!input) return;
      // pick === 'file' -> file picker; default is still directory (preserving existing behavior)
      const fileMode = btn.dataset.setPick === "file";
      try {
        const picked = await open({ directory: !fileMode, multiple: false });
        if (typeof picked === "string") input.value = picked;
      } catch {
        /* dialog unavailable */
      }
    });
  });

  box.querySelectorAll<HTMLButtonElement>("button[data-set-action]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const pair = btn.dataset.setAction ?? "";
      const [id, key] = splitSettingPair(pair);
      const msg = msgOf(pair);
      try {
        await invoke("invoke_plugin_setting_action", { id, key });
        if (msg) {
          msg.textContent = t("pluginSettingActionDone");
          msg.classList.remove("cfg-err");
        }
      } catch (err) {
        if (msg) {
          msg.textContent = `${t("pluginConfigSaveFailed")}: ${(err as Error).message ?? err}`;
          msg.classList.add("cfg-err");
        }
      }
    });
  });

  // ── P3 list control: forward op -> plugin returns a new array -> repaint that list ──────────────────
  box.querySelectorAll<HTMLElement>("[data-list-body]").forEach((bodyEl) => {
    // Host row = the .setting-row containing data-list-body
    const row = bodyEl.closest(".setting-row") as HTMLElement | null;
    if (!row) return;
    const pair = bodyEl.dataset.listBody ?? "";
    const [id, key] = splitSettingPair(pair);
    // The result only writes an inline hint: failure takes the same path as other declarative controls — a modal here would yank the user off the current page.
    // Success clears the previous error (the repainted list itself is the receipt).
    const say = (text: string, isErr: boolean): void => {
      const msg = msgOf(pair);
      if (!msg) return;
      msg.textContent = text;
      msg.classList.toggle("cfg-err", isErr);
    };
    // Don't send again until the previous send returns: the button's index is from paint time, the list has changed, and sending again would hit a different item
    // (double-clicking x quickly would delete two; repeated up/down would undo the move just made). op takes up to 10s (the plugin may be lazy-starting).
    let busy = false;
    // When it fails before the first frame is painted (plugin won't start / timeout), clear the 'Loading…' placeholder,
    // otherwise a row is stuck in the loading state forever and the page has no retry entry. Use a flag instead of probing the DOM:
    // an empty list is also a legitimate result with no .kv-row painted, and probing the DOM would wrongly clear '(empty list)'.
    let painted = false;
    const call = async (payload: Record<string, unknown>): Promise<boolean> => {
      if (busy) return false;
      busy = true;
      try {
        const r = await invoke<unknown[]>("invoke_plugin_setting_list", { id, key, ...payload });
        paint(Array.isArray(r) ? r : []);
        say("", false);
        return true;
      } catch (err) {
        if (!painted) bodyEl.innerHTML = "";
        say(`${t("pluginConfigSaveFailed")}: ${(err as Error).message ?? err}`, true);
        return false;
      } finally {
        busy = false;
      }
    };
    const paint = (items: unknown[]): void => {
      painted = true;
      // The first row's up and last row's down are always disabled: to is Option<usize>, and to=-1 fails to deserialize,
      // to=len is a silent no-op on the plugin side — out-of-bounds moves are never sent. When the row is turned off by the disable predicate, all are disabled.
      const rowOff = row.classList.contains("cfg-row-off");
      // aria-label: up / down / x read out alone have no semantics (x is read as 'multiplication sign').
      bodyEl.innerHTML =
        items.length === 0
          ? `<span class="setting-hint">${esc(t("pluginSettingListEmpty"))}</span>`
          : items
              .map(
                (v, i) => `<div class="kv-row"><span class="kv-idx">${i + 1}</span><span class="kv-val">${esc(String(v))}</span><span class="list-ops"><button class="btn ghost" data-list-up="${i}" type="button" aria-label="${escAttr(t("pluginSettingListMoveUp"))}"${rowOff || i === 0 ? " disabled" : ""}>↑</button><button class="btn ghost" data-list-down="${i}" type="button" aria-label="${escAttr(t("pluginSettingListMoveDown"))}"${rowOff || i === items.length - 1 ? " disabled" : ""}>↓</button><button class="btn ghost danger" data-list-del="${i}" type="button" aria-label="${escAttr(t("pluginSettingListDelete"))}"${rowOff ? " disabled" : ""}>×</button></span></div>`,
              )
              .join("");
      bodyEl.querySelectorAll<HTMLButtonElement>("[data-list-up]").forEach((b) =>
        b.addEventListener("click", () => void call({ op: "move", index: Number(b.dataset.listUp), to: Number(b.dataset.listUp) - 1 })),
      );
      bodyEl.querySelectorAll<HTMLButtonElement>("[data-list-down]").forEach((b) =>
        b.addEventListener("click", () => void call({ op: "move", index: Number(b.dataset.listDown), to: Number(b.dataset.listDown) + 1 })),
      );
      bodyEl.querySelectorAll<HTMLButtonElement>("[data-list-del]").forEach((b) =>
        b.addEventListener("click", () => void call({ op: "delete", index: Number(b.dataset.listDel) })),
      );
    };
    row.querySelector(`button[data-list-add="${cssEscape(pair)}"]`)?.addEventListener("click", async () => {
      const input = row.querySelector<HTMLInputElement>("[data-list-new]");
      const v = input?.value.trim();
      if (!v) return; // empty/whitespace-only: don't send (the plugin rejects it too, but no need to round-trip)
      // Clear only on success, and only the part just submitted: a failed value stays in the box for the user to fix (same as saveDeclaredSetting);
      // if the user has already started typing the next one while waiting, that new input must not be wiped by this success response.
      if ((await call({ op: "add", value: v })) && input && input.value.trim() === v) input.value = "";
    });
    void call({ op: "list" });
  });
}
