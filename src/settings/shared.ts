import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { disable, enable } from "@tauri-apps/plugin-autostart";
import { setLocale, t, type I18nKey, type Locale } from "../i18n";
import type { AgentEvent } from "../shared";
import {
  ICON_AGENTS,
  ICON_AUDIT,
  ICON_AUTOMATION,
  ICON_BUBBLE,
  ICON_CONFIG,
  ICON_GENERAL,
  ICON_LOGS,
  ICON_NOTIFY,
  ICON_PET,
  ICON_PLUGINS,
  ICON_SLA,
  ICON_STATS,
} from "../icons";
import {
  BUBBLE_DENSITIES,
  BUBBLE_POSITIONS,
  BUBBLE_THEMES,
} from "../bubble";
import { setSoundEnabled, type SoundKind } from "../sounds";
import type { PluginConfigRow, PluginSettingsView } from "./types";

export interface AppSettings {
  theme: string;
  opacity: number;
  fontSize: number;
  mode: string;
  soundDone: boolean;
  soundWaiting: boolean;
  locale: Locale;
  petSize: number;
  maxRows: number;
  bubbleTheme: string;
  /** Which side of the pet the bubble sits on: right / left / top / bottom. */
  bubblePos: string;
  /** Space between the pet frame and the bubble, logical px 0..24. */
  bubbleGap: number;
  /** Bubble information density: tight / standard / rich. */
  bubbleDensity: string;
  petSheet: string;
  /** Selected petpack id (~/.opencapx/pets/<id>); empty = use spritesheet / built-in logo. */
  petPack: string;
  bubbleEnabled: boolean;
  bubbleDuration: number;
  petVisible: boolean;
  onboarded: boolean;
  breakEnabled: boolean;
  breakMinutes: number;
  /// SessionStart additionalContext injection: capability digest into supported agents'
  /// context at session start (Rust side reads this in http::session_start_reply).
  sessionContextInject?: boolean;
  /// i2 §13 — epoch seconds of the last 'mark all read'; notification.posted after this counts as unread.
  notifLastRead?: number;
}

export const DEFAULTS: AppSettings = {
  theme: "system",
  opacity: 0.9,
  fontSize: 13,
  mode: "carousel",
  soundDone: true,
  soundWaiting: true,
  locale: "en",
  petSize: 100,
  maxRows: 5,
  bubbleTheme: "chef",
  bubblePos: "right",
  bubbleGap: 0,
  bubbleDensity: "standard",
  petSheet: "",
  petPack: "",
  bubbleEnabled: true,
  bubbleDuration: 5,
  petVisible: true,
  onboarded: false,
  breakEnabled: false,
  breakMinutes: 60,
  sessionContextInject: true,
  notifLastRead: 0,
};

export type Tab =
  | "general"
  | "pet"
  | "bubble"
  | "plugins"
  | "market"
  | "agents"
  | "rpcTrace"
  | "capabilities"
  | "audit"
  | "notify"
  | "automation"
  | "rules"
  | "logs"
  | "stats"
  | "sla"
  | "hotkeys"
  | "backup"
  | "profiles"
  | "metrics"
  | "alerting"
  | `plugin:${string}`;

export const TABS: Array<{ id: Tab; labelKey: I18nKey; icon: string }> = [
  { id: "general", labelKey: "tabGeneral", icon: ICON_GENERAL },
  { id: "pet", labelKey: "tabPet", icon: ICON_PET },
  { id: "bubble", labelKey: "tabBubble", icon: ICON_BUBBLE },
  { id: "agents", labelKey: "tabAgents", icon: ICON_AGENTS },
  { id: "rpcTrace", labelKey: "rpcTraceTitle", icon: ICON_AUDIT },
  { id: "capabilities", labelKey: "tabCapabilities", icon: ICON_CONFIG },
  { id: "audit", labelKey: "tabAudit", icon: ICON_AUDIT },
  { id: "notify", labelKey: "tabNotify", icon: ICON_NOTIFY },
  { id: "automation", labelKey: "tabAutomation", icon: ICON_AUTOMATION },
  { id: "rules", labelKey: "tabRules", icon: ICON_AUTOMATION },
  { id: "logs", labelKey: "tabLogs", icon: ICON_LOGS },
  { id: "stats", labelKey: "tabStats", icon: ICON_STATS },
  { id: "sla", labelKey: "tabSla", icon: ICON_SLA },
  { id: "hotkeys", labelKey: "tabHotkeys", icon: ICON_PLUGINS },
  { id: "backup", labelKey: "tabBackup", icon: ICON_CONFIG },
  { id: "profiles", labelKey: "tabProfiles", icon: ICON_CONFIG },
  { id: "metrics", labelKey: "tabMetrics", icon: ICON_STATS },
  { id: "alerting", labelKey: "tabAlerting", icon: ICON_SLA },
  { id: "market", labelKey: "tabMarket", icon: ICON_PLUGINS },
  { id: "plugins", labelKey: "tabPlugins", icon: ICON_PLUGINS },
];

let settings: AppSettings = { ...DEFAULTS };
let tab: Tab = "general";
let appVersion = "";
let sessions: AgentEvent[] = [];
let notifUnread = 0;
let freshTab = false;
let windowFocused = true;

export function getSettings(): AppSettings {
  return settings;
}

/** Replace the in-memory settings snapshot. */
export function setSettings(next: AppSettings): void {
  settings = next;
}

export function getTab(): Tab {
  return tab;
}

export function setTab(next: Tab): void {
  tab = next;
}

export function getAppVersion(): string {
  return appVersion;
}

export function setAppVersion(next: string): void {
  appVersion = next;
}

export function getSessions(): AgentEvent[] {
  return sessions;
}

export function setSessions(next: AgentEvent[]): void {
  sessions = next;
}

export function getNotifUnread(): number {
  return notifUnread;
}

export function setNotifUnread(next: number): void {
  notifUnread = next;
}

export function getFreshTab(): boolean {
  return freshTab;
}

export function setFreshTab(next: boolean): void {
  freshTab = next;
}

export function getWindowFocused(): boolean {
  return windowFocused;
}

export function setWindowFocused(next: boolean): void {
  windowFocused = next;
  paintFocus();
}

export function paintFocus(): void {
  document.querySelector(".op-settings")?.classList.toggle("unfocused", !windowFocused);
}

export function esc(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

/// Attribute value escaping: esc only guards &<> — quotes in values and text would truncate `value='…'`, so guard again inside the attribute
/// (same convention as the config index's haystack).
export function escAttr(s: string): string {
  return esc(s).replace(/"/g, "&quot;");
}

export function applyTheme(): void {
  const root = document.documentElement;
  root.classList.toggle("light", settings.theme === "light");
  root.classList.toggle("dark", settings.theme === "dark");
}

export function startThemeListener(): void {
  if (window.matchMedia) {
    window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
      if (settings.theme === "system") applyTheme();
    });
  }
}

export async function load(): Promise<void> {
  try {
    const saved = await invoke<Partial<AppSettings>>("get_settings");
    settings = { ...DEFAULTS, ...saved };
  } catch {
    settings = { ...DEFAULTS };
  }
  // Stored values can be stale or hand-edited: fall back per field so every picker always shows
  // one active option. The overlay validates the same way on its side when it reads the settings.
  if (!(BUBBLE_POSITIONS as readonly string[]).includes(settings.bubblePos)) settings.bubblePos = DEFAULTS.bubblePos;
  settings.bubbleGap = clampInt(String(settings.bubbleGap), 0, 24, DEFAULTS.bubbleGap);
  if (!(BUBBLE_THEMES as readonly string[]).includes(settings.bubbleTheme)) settings.bubbleTheme = DEFAULTS.bubbleTheme;
  if (!(BUBBLE_DENSITIES as readonly string[]).includes(settings.bubbleDensity)) settings.bubbleDensity = DEFAULTS.bubbleDensity;
  // Same literal list as the segmented control; the overlay falls back to "carousel" on its side.
  if (!["list", "carousel", "compact", "focus"].includes(settings.mode)) settings.mode = DEFAULTS.mode;
  // Same bounds as the overlay: a value written by an older build (or a hand-edited file) shows
  // clamped here instead of displaying a number that silently behaves as a different one.
  settings.maxRows = clampInt(String(settings.maxRows), 1, 10, DEFAULTS.maxRows);
  settings.bubbleDuration = clampInt(String(settings.bubbleDuration), 0, 300, DEFAULTS.bubbleDuration);
  settings.breakMinutes = clampInt(String(settings.breakMinutes), 5, 480, DEFAULTS.breakMinutes);
  setLocale(settings.locale);
  applyTheme();
}

export async function save(): Promise<void> {
  try {
    await invoke("set_settings", { value: settings });
  } catch {
    /* settings file unavailable, keep in-memory */
  }
}

export async function refreshSessions(): Promise<void> {
  try {
    sessions = await invoke<AgentEvent[]>("get_sessions");
  } catch {
    sessions = [];
  }
  paintSessions();
}

export function row(labelKey: I18nKey, hintKey: I18nKey | null, control: string): string {
  const hint = hintKey ? `<span class="setting-hint">${esc(t(hintKey))}</span>` : "";
  return `<div class="setting-row"><div class="setting-info"><span class="setting-label">${esc(t(labelKey))}</span>${hint}</div>${control}</div>`;
}

/// Number inputs clamp to the same bounds the overlay enforces on read, so the stored value and the
/// effective value can't drift apart (typing 999 or clearing the field used to save the raw number).
export function clampInt(raw: string, min: number, max: number, fallback: number): number {
  const n = Math.round(Number(raw));
  return Number.isFinite(n) ? Math.min(max, Math.max(min, n)) : fallback;
}

export function group(titleKey: I18nKey | null, inner: string): string {
  const title = titleKey ? `<p class="settings-group-title">${esc(t(titleKey))}</p>` : "";
  return `${title}<div class="settings-list">${inner}</div>`;
}

export function toggle(controlKey: string, on: boolean): string {
  return `<button class="toggle-switch${on ? " active" : ""}" data-toggle="${controlKey}" type="button"><span class="toggle-slider"></span></button>`;
}

export function segmented(
  name: string,
  options: string[],
  current: string,
  labels?: Partial<Record<string, I18nKey>>,
): string {
  return `<div class="segmented-control">${options
    .map((o) => `<button class="segment-btn${o === current ? " active" : ""}" data-seg="${name}" data-val="${esc(o)}" type="button">${esc(labels?.[o] ? t(labels[o] as I18nKey) : o)}</button>`)
    .join("")}</div>`;
}

export function paintSessions(): void {
  const box = document.getElementById("session-list");
  if (!box) return;
  if (sessions.length === 0) {
    box.innerHTML = `<p class="empty">${esc(t("noSessions"))}</p>`;
    return;
  }
  box.innerHTML = sessions
    .map(
      (s) =>
        `<div class="sess"><span class="dot ${esc(s.state)}"></span><b>${esc(s.agent)}</b><span>${esc(s.project)}</span><span class="msg">${esc(s.message)}</span><button data-id="${esc(s.id)}" type="button">${esc(t("dismiss"))}</button></div>`,
    )
    .join("");
  box.querySelectorAll("button[data-id]").forEach((b) => {
    b.addEventListener("click", async () => {
      await invoke("dismiss_session", { id: (b as HTMLElement).dataset.id });
      await refreshSessions();
    });
  });
}

export function bindToggles(body: HTMLElement): void {
  body.querySelectorAll("button[data-toggle]").forEach((b) => {
    b.addEventListener("click", () => {
      const key = (b as HTMLElement).dataset.toggle ?? "";
      const on = !b.classList.contains("active");
      b.classList.toggle("active", on);
      if (key === "soundDone" || key === "soundWaiting") {
        const kind = (key === "soundDone" ? "done" : "waiting") as SoundKind;
        setSoundEnabled(kind, on);
        setSettings({ ...settings, [key]: on });
        void save();
      } else if (key === "bubbleEnabled") {
        setSettings({ ...settings, bubbleEnabled: on });
        void save();
      } else if (key === "petVisible") {
        setSettings({ ...settings, petVisible: on });
        void save();
      } else if (key === "breakEnabled") {
        setSettings({ ...settings, breakEnabled: on });
        void save();
      } else if (key === "sessionContextInject") {
        setSettings({ ...settings, sessionContextInject: on });
        void save();
      } else if (key === "autostart") {
        if (on) void enable().catch(() => undefined);
        else void disable().catch(() => undefined);
      }
    });
  });
}

/// Snapshot of the last fetched plugin config: the sidebar 'Plugin Settings' section and the per-plugin settings page share the same data
/// (from the existing list_plugin_config + list_plugin_settings paths); no new command, no separate derivation.
export type PluginConfigSnapshot = {
  plugins: PluginConfigRow[];
  views: Map<string, PluginSettingsView>;
};

let pluginCfgSnap: PluginConfigSnapshot = {
  plugins: [],
  views: new Map(),
};

/// Plugin id -> display name: from list_plugins (fetched by both refreshPlugins and refreshPluginConfig),
/// shared by sidebar labels and plugin settings page titles; falls back to the id only when the name is unknown.
let pluginNameById = new Map<string, string>();

/// Back target for the plugin settings page: only recorded when entered from the plugins detail card; entering from the sidebar leaves it null
/// (the sidebar section is itself the entry point, so no Back is placed on the page). Cleared automatically when switchTab goes to a non-plugin: tab.
let pluginPageReturn: "plugins" | null = null;

export function getPluginConfigSnapshot(): PluginConfigSnapshot {
  return pluginCfgSnap;
}

export function setPluginConfigSnapshot(next: PluginConfigSnapshot): void {
  pluginCfgSnap = next;
}

export function getPluginNameById(): ReadonlyMap<string, string> {
  return pluginNameById;
}

export function setPluginNameById(next: Map<string, string>): void {
  pluginNameById = next;
}

export function getPluginPageReturn(): "plugins" | null {
  return pluginPageReturn;
}

export function setPluginPageReturn(next: "plugins" | null): void {
  pluginPageReturn = next;
}

/// Bad-database quarantine record (Rust writes ~/.opencapx/db-recovery.json when the DB fails to open or validate).
/// Read once at startup; if present, a persistent notice sits at the top of the content area — making 'the database was reset' visible
/// instead of letting the app run silently in memory as if everything were fine.
export interface DbRecoveryNotice {
  from: string;
  to: string;
  reason: string;
  at: number;
}

let dbRecovery: DbRecoveryNotice | null = null;

export function getDbRecovery(): DbRecoveryNotice | null {
  return dbRecovery;
}

export function setDbRecovery(next: DbRecoveryNotice | null): void {
  dbRecovery = next;
}

/// Read once at startup (failure/absent -> null; don't bother the user).
export async function loadDbRecoveryNotice(): Promise<void> {
  try {
    dbRecovery = await invoke<DbRecoveryNotice | null>("db_recovery_notice");
  } catch {
    dbRecovery = null;
  }
}

/// Sidebar 'Plugin Settings' section: one entry per plugin (label = display name), those declaring settings[] first,
/// those not declaring (using the KV/JSON editor) after; the whole section is not rendered with zero plugins. Clicking an entry goes through switchTab; the sidebar
/// is itself the entry point, so no Back is set.
/// Defined before all call sites (render / refreshPluginConfig) — so it is never 'undefined' regardless of load order.
export function renderPluginSettingsNav(): void {
  const host = document.getElementById("plugin-nav-section");
  if (!host) return;
  const entries = [...pluginCfgSnap.plugins].sort((a, b) => {
    const da = pluginCfgSnap.views.has(a.id) ? 0 : 1;
    const db = pluginCfgSnap.views.has(b.id) ? 0 : 1;
    if (da !== db) return da - db;
    return (pluginNameById.get(a.id) ?? a.id).localeCompare(pluginNameById.get(b.id) ?? b.id);
  });
  if (entries.length === 0) {
    host.innerHTML = "";
    return;
  }
  host.innerHTML =
    `<p class="settings-group-title nav-section-title">${esc(t("pluginSettingsSection"))}</p>` +
    entries
      .map((p) => {
        const id = `plugin:${p.id}`;
        const active = tab === id;
        return `<button class="nav-item${active ? " active" : ""}" data-tab="${esc(id)}" type="button"><span class="nav-icon">${ICON_PLUGINS}</span><span class="nav-label">${esc(pluginNameById.get(p.id) ?? p.id)}</span>${active ? '<span class="active-indicator"></span>' : ""}</button>`;
      })
      .join("");
  host.querySelectorAll("button[data-tab]").forEach((b) => {
    b.addEventListener("click", () => {
      // Sidebar entries provide no Back: clear the return target left by a previous entry from the detail card
      pluginPageReturn = null;
      switchTab((b as HTMLElement).dataset.tab as Tab);
    });
  });
}

export interface TabModule {
  id: Tab | "plugin:";
  render(body: HTMLElement): void;
  stop?(): void;
}

const tabModules = new Map<Tab | "plugin:", TabModule>();
let legacyRenderer: ((body: HTMLElement) => void) | null = null;
let legacyCleanup: (() => void) | null = null;

export function registerTab(module: TabModule): void {
  tabModules.set(module.id, module);
}

/** Temporary migration bridge: the entry keeps the old branch chain until each domain registers. */
export function registerLegacyRenderer(renderer: (body: HTMLElement) => void): void {
  legacyRenderer = renderer;
}

/** Temporary migration bridge for the four existing tab-cleanup callbacks. */
export function registerLegacyCleanup(cleanup: () => void): void {
  legacyCleanup = cleanup;
}

function findTabModule(next: Tab): TabModule | undefined {
  const exact = tabModules.get(next);
  if (exact) return exact;
  if (next.startsWith("plugin:")) return tabModules.get("plugin:");
  return undefined;
}

/// The single entry point for switching tabs (shared by sidebar static items and the 'Plugin Settings' section): stop streams + mark freshTab, then render.
/// There is only this one entry point, so the plugin settings section buttons and static items cannot drift apart.
export function switchTab(next: Tab): void {
  findTabModule(tab)?.stop?.();
  legacyCleanup?.();
  setFreshTab(true); // enter animation plays only on tab switch; redraws from settings changes do not play it
  // Drop the Back target when leaving a plugin settings page, so the next sidebar entry doesn't leave a stale Back pointing at an old tab
  if (!next.startsWith("plugin:")) setPluginPageReturn(null);
  tab = next;
  render();
}

export function render(): void {
  const root = document.getElementById("settings");
  if (!root) return;
  const activeTab = tab;
  const unread = notifUnread;
  const recovery = dbRecovery;
  root.innerHTML = `
    <div class="op-settings">
      <aside class="op-sidebar">
        <div class="sidebar-header">
          <div class="drag-handle" data-tauri-drag-region></div>
          <div class="window-controls">
            <button class="control-btn close" id="win-close" type="button" aria-label="Close"><svg viewBox="0 0 24 24" width="8" height="8" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round"><path d="M18 6 6 18M6 6l12 12"/></svg></button>
            <button class="control-btn minimize" id="win-min" type="button" aria-label="Minimize"><svg viewBox="0 0 24 24" width="8" height="8" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round"><path d="M5 12h14"/></svg></button>
            <button class="control-btn maximize" id="win-max" type="button" aria-label="Maximize"><svg viewBox="0 0 24 24" width="8" height="8" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M8 3H5a2 2 0 0 0-2 2v3M16 3h3a2 2 0 0 1 2 2v3M8 21H5a2 2 0 0 1-2-2v-3M16 21h3a2 2 0 0 0 2-2v-3"/></svg></button>
          </div>
        </div>
        <nav>${TABS.map((tb) => `<button class="nav-item${activeTab === tb.id ? " active" : ""}" data-tab="${tb.id}" type="button"><span class="nav-icon">${tb.icon}</span><span class="nav-label">${esc(t(tb.labelKey))}</span>${tb.id === "notify" && unread > 0 ? `<span class="nav-badge">${unread > 99 ? "99+" : unread}</span>` : ""}${activeTab === tb.id ? '<span class="active-indicator"></span>' : ""}</button>`).join("")}<div id="plugin-nav-section"></div></nav>
      </aside>
      <main class="op-content">${recovery ? `<div class="db-recovery-banner" role="alert"><div class="db-recovery-text"><strong>${esc(t("dbRecoveryTitle"))}</strong><span>${esc(t("dbRecoveryBody").replace("{reason}", recovery.reason).replace("{path}", recovery.to))}</span></div><button class="btn ghost" id="db-recovery-dismiss" type="button">${esc(t("dbRecoveryDismiss"))}</button></div>` : ""}<div class="content-section" id="tab-body"></div></main>
    </div>`;
  root.querySelectorAll("button[data-tab]").forEach((b) => {
    b.addEventListener("click", () => switchTab((b as HTMLElement).dataset.tab as Tab));
  });
  document.getElementById("win-close")?.addEventListener("click", () => void getCurrentWindow().hide());
  document.getElementById("win-min")?.addEventListener("click", () => void getCurrentWindow().minimize());
  document.getElementById("win-max")?.addEventListener("click", () => {
    const win = getCurrentWindow();
    void win.isMaximized().then((m) => (m ? win.unmaximize() : win.maximize()));
  });
  paintFocus();
  document.getElementById("db-recovery-dismiss")?.addEventListener("click", async () => {
    try {
      await invoke("dismiss_db_recovery_notice");
    } catch {
      /* Even if deletion fails, collapse the banner first: the notice should appear only once */
    }
    dbRecovery = null;
    render();
  });
  // Plugin settings section: redrawn on every render (data comes from the pluginCfgSnap snapshot)
  renderPluginSettingsNav();
  const body = document.getElementById("tab-body");
  if (!body) return;
  // Play the enter animation only on the render for a tab switch (avoid replaying on every settings change)
  body.classList.toggle("fresh-tab", freshTab);
  freshTab = false;

  // Plugin settings page: if the plugin was uninstalled -> fall back to the plugin list, don't stay on an empty page.
  // Plugins without a declared settings[] also have a settings page (a config editor there), so only check whether the plugin exists.
  if (tab.startsWith("plugin:") && !pluginCfgSnap.plugins.some((p) => p.id === tab.slice("plugin:".length))) {
    tab = "plugins";
  }

  const module = findTabModule(tab);
  if (module) {
    module.render(body);
  } else {
    legacyRenderer?.(body);
  }
}

/// Open a plugin's settings page (the detail card / sidebar both land on the same page).
export function openPluginSettingsPage(id: string, from: "plugins"): void {
  pluginPageReturn = from;
  switchTab(`plugin:${id}`);
}

/// Plugin settings snapshot: the sidebar 'Plugin Settings' section and the per-plugin settings page share the same data
/// (from the existing list_plugin_config + list_plugin_settings paths); no new command.
export async function refreshPluginConfig(): Promise<void> {
  let snap: { plugins: PluginConfigRow[] } = { plugins: [] };
  try {
    const raw = await invoke<{ plugins: PluginConfigRow[] } | null>("list_plugin_config");
    // The backend or a test stub may return null / missing fields: accept only responses with a plugins array
    if (raw && Array.isArray(raw.plugins)) snap = raw;
  } catch {
    /* commands unavailable */
  }
  // Declaration view: badges need to count declared keys, and the plugin settings page also consumes this cache
  const views = new Map<string, PluginSettingsView>();
  await Promise.all(
    snap.plugins.map(async (p) => {
      try {
        const v = await invoke<PluginSettingsView>("list_plugin_settings", { id: p.id });
        if (v && Array.isArray(v.settings) && v.settings.length > 0) views.set(p.id, v);
      } catch {
        /* no declarations */
      }
    })
  );
  // Display name: list_plugins is the only source; if unavailable, keep the existing mapping (labels fall back to id)
  try {
    const plugins = await invoke<Array<{ id: string; name: string }> | null>("list_plugins");
    if (Array.isArray(plugins)) pluginNameById = new Map(plugins.map((p) => [p.id, p.name]));
  } catch {
    /* Name unavailable: keep the existing mapping */
  }
  // Update the cache + sidebar 'Plugin Settings' section first: even when not currently on this tab, entries must follow install/uninstall/update
  pluginCfgSnap = { plugins: snap.plugins, views };
  renderPluginSettingsNav();
  // Currently sitting on a plugin settings page: re-render the body with the new data (after saving, predicates/validation recompute accordingly)
  if (tab.startsWith("plugin:")) render();
}

/** CSS.escape isn't always available (old browsers); fall back to regex replacement. */
export function cssEscape(s: string): string {
  if (typeof (window as unknown as { CSS?: { escape?: (s: string) => string } }).CSS?.escape === "function") {
    return (window as unknown as { CSS: { escape: (s: string) => string } }).CSS.escape(s);
  }
  return s.replace(/[^a-zA-Z0-9_-]/g, "\\$&");
}

/// List first-load skeleton: outlines for `cards` cards (header + 3 row bars), reusing the audit-pulse breathing animation, no new keyframes.
/// Audit / Automation / Replay / System Capabilities all share the same set of .list-skel-* styles.
export function listSkeleton(cards: number): string {
  const card = `<div class="list-skel-card"><div class="list-skel-bar list-skel-head" aria-hidden="true"></div><div class="list-skel-bar" aria-hidden="true"></div><div class="list-skel-bar" aria-hidden="true"></div><div class="list-skel-bar" aria-hidden="true"></div></div>`;
  return card.repeat(cards);
}

/// List load failure block: short message + retry button; failure != empty, don't pass empty-state text off as it.
export function listError(msgKey: I18nKey, retryId: string): string {
  return `<div class="list-error"><span class="list-error-text">${esc(t(msgKey))}</span><button class="btn ghost" id="${esc(retryId)}" type="button">${esc(t("auditRefresh"))}</button></div>`;
}

/// Shared 'label + value' detail item for collapsible rows; an empty value is not rendered (don't show everything).
export function detailKv(label: string, value: string): string {
  if (!value) return "";
  return `<div class="kv"><span class="kv-k">${esc(label)}</span><span class="kv-v">${esc(value)}</span></div>`;
}

/// Shared multi-line text block for collapsible rows (raw text, not JSON-serialized).
export function detailBlock(label: string, text: string): string {
  if (!text) return "";
  return `<div class="kv"><span class="kv-k">${esc(label)}</span><pre class="kv-v kv-payload">${esc(text)}</pre></div>`;
}

/// JSON details (used for timeline's raw payload).
export function detailPayload(label: string, value: unknown): string {
  let text = "";
  try {
    text = JSON.stringify(value, null, 2) ?? "";
  } catch {
    text = String(value);
  }
  if (text === "{}" || text === "null") return "";
  return detailBlock(label, text);
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(2)} MB`;
}

export function formatLifecycleTime(ts: number): string {
  if (!ts) return "—";
  try {
    const d = new Date(ts * 1000);
    return d.toLocaleString();
  } catch {
    return String(ts);
  }
}

/// Today / yesterday / localized date. timestamp is epoch seconds.
export function dayLabel(ts: number): string {
  const d = new Date(ts * 1000);
  const today = new Date();
  const yesterday = new Date(today);
  yesterday.setDate(today.getDate() - 1);
  const sameDay = (a: Date, b: Date): boolean =>
    a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
  if (sameDay(d, today)) return t("timelineToday");
  if (sameDay(d, yesterday)) return t("timelineYesterday");
  return d.toLocaleDateString();
}
