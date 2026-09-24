import { invoke } from "@tauri-apps/api/core";
import { isEnabled } from "@tauri-apps/plugin-autostart";
import { availableLocales, getLocale, setLocale, t, type Locale } from "../i18n";
import { ICON_PET } from "../icons";
import { isSoundEnabled, setCustomSound, type SoundKind } from "../sounds";
import { refreshPlugins } from "./plugins";
import {
  applyTheme,
  bindToggles,
  esc,
  getAppVersion,
  getSettings,
  group,
  paintSessions,
  refreshSessions,
  render,
  row,
  save,
  segmented,
  setSettings,
  toggle,
} from "./shared";

interface AgentInfo {
  kind: string;
  display_name: string;
  installed: boolean;
  note: string | null;
}

let agents: AgentInfo[] = [];

export function renderGeneral(body: HTMLElement): void {
  // The welcome card used to clear this flag; keep clearing it here so the first-run auto-open (overlay.ts) stays one-time
  if (!getSettings().onboarded) {
    setSettings({ ...getSettings(), onboarded: true });
    void save();
  }
  body.innerHTML =
    `<div class="settings-list"><div class="about-card"><div class="logo">${ICON_PET}</div><div><b>OpenCapX</b></div><div class="ver">${esc(t("version"))} ${esc(getAppVersion())}</div><p>${esc(t("aboutText"))}</p></div></div>` +
    group("agents", `<div class="setting-row vertical"><span class="setting-hint">${esc(t("agentsHint"))}</span><div id="agent-list"></div></div>`) +
    group("sessions", `<div class="setting-row vertical"><span class="setting-hint">${esc(t("sessionsHint"))}</span><div id="session-list"></div><div><button class="btn ghost" id="clear" type="button">${esc(t("clearAll"))}</button></div></div>`) +
    group("sounds",
      row("soundDone", "soundDoneHint", toggle("soundDone", isSoundEnabled("done"))) +
      row("soundWaiting", "soundWaitingHint", toggle("soundWaiting", isSoundEnabled("waiting"))) +
      `<div class="setting-row vertical"><div class="scenarios"><label class="btn ghost" for="snd-done-file">${esc(t("soundUpload"))} · ${esc(t("soundDone"))}</label><input type="file" id="snd-done-file" accept="audio/*" hidden /><button class="btn ghost" id="snd-done-reset" type="button">${esc(t("soundReset"))}</button><label class="btn ghost" for="snd-wait-file">${esc(t("soundUpload"))} · ${esc(t("soundWaiting"))}</label><input type="file" id="snd-wait-file" accept="audio/*" hidden /><button class="btn ghost" id="snd-wait-reset" type="button">${esc(t("soundReset"))}</button></div></div>`) +
    group(null,
      row("autostart", "autostartHint", toggle("autostart", false)) +
      // Language list auto-generated: adding src/locales/<code>.json makes it appear here
      row(
        "locale",
        "localeHint",
        `<select id="locale">${availableLocales()
          .map((l) => `<option value="${esc(l.code)}">${esc(l.name)}</option>`)
          .join("")}</select>`,
      ) +
      row("theme", "themeHint", segmented("theme", ["light", "dark", "system"], getSettings().theme)) +
      row("breakReminder", "breakReminderHint", toggle("breakEnabled", getSettings().breakEnabled)) +
      row("breakMinutes", null, `<input type="number" id="bmins" min="5" max="480" value="${getSettings().breakMinutes}" />`) +
      row("sessionContext", "sessionContextHint", toggle("sessionContextInject", getSettings().sessionContextInject !== false)) +
      row("channelDefault", "channelDefaultHint", `<select id="default-channel"><option value="stable">${esc(t("channelStable"))}</option><option value="beta">${esc(t("channelBeta"))}</option><option value="dev">${esc(t("channelDev"))}</option></select><span class="setting-hint" id="channel-msg"></span>`)) +
    group("cliCommand", `<div class="setting-row vertical"><span class="setting-hint">${esc(t("cliCommandHint"))}</span><div class="ks-status" id="cli-status"></div><div><button class="btn ghost" id="cli-toggle" type="button"></button><span class="setting-hint" id="cli-msg"></span></div></div>`) +
    group("killSwitch", `<div class="setting-row vertical"><span class="setting-hint">${esc(t("killSwitchHint"))}</span><div class="ks-status" id="ks-status"></div><div class="ks-controls"><input type="text" id="ks-reason" placeholder="${esc(t("killSwitchReasonPlaceholder"))}" maxlength="120"/><button class="btn danger" id="ks-enable" type="button">${esc(t("killSwitchEnable"))}</button><button class="btn ghost" id="ks-disable" type="button">${esc(t("killSwitchDisable"))}</button></div></div>`) +
    group("safeMode", `<div class="setting-row vertical"><span class="setting-hint">${esc(t("safeModeHint"))}</span><div class="ks-status" id="sm-status"></div></div>`);
  paintSessions();
  void refreshAgents();
  void refreshCliCommand();
  document.getElementById("cli-toggle")?.addEventListener("click", async () => {
    const btn = document.getElementById("cli-toggle") as HTMLButtonElement;
    const msg = document.getElementById("cli-msg");
    btn.disabled = true;
    try {
      const out = await invoke<string>(btn.dataset.mode === "uninstall" ? "cli_command_uninstall" : "cli_command_install");
      if (msg) msg.textContent = out;
    } catch (err) {
      if (msg) msg.textContent = `✗ ${String(err)}`;
    } finally {
      btn.disabled = false;
      void refreshCliCommand();
    }
  });
  document.getElementById("clear")?.addEventListener("click", async () => {
    await invoke("clear_sessions");
    await refreshSessions();
  });
  bindToggles(body);
  wireSoundUpload("snd-done-file", "snd-done-reset", "done");
  wireSoundUpload("snd-wait-file", "snd-wait-reset", "waiting");
  const loc = document.getElementById("locale") as HTMLSelectElement | null;
  if (loc) {
    loc.value = getLocale();
    loc.addEventListener("change", () => {
      const v = loc.value as Locale;
      setLocale(v);
      setSettings({ ...getSettings(), locale: v });
      void save();
      render();
    });
  }
  const auto = body.querySelector('button[data-toggle="autostart"]');
  if (auto) {
    isEnabled().then((v) => auto.classList.toggle("active", v)).catch(() => undefined);
  }
  // Phase 38 — global default update channel
  const dc = document.getElementById("default-channel") as HTMLSelectElement | null;
  if (dc) {
    invoke<string>("get_default_channel")
      .then((cur) => {
        dc.value = cur || "stable";
      })
      .catch(() => {
        dc.value = "stable";
      });
    dc.addEventListener("change", () => {
      const prev = dc.value;
      const msg = document.getElementById("channel-msg");
      invoke("set_default_channel", { channel: dc.value }).then(
        () => { if (msg) msg.textContent = ""; },
        (err) => {
          dc.value = prev;
          if (msg) msg.textContent = `${t("channelSetFailed")}: ${(err as Error).message ?? err}`;
        },
      );
    });
  }
  body.querySelectorAll("button[data-seg='theme']").forEach((b) => {
    b.addEventListener("click", () => {
      setSettings({ ...getSettings(), theme: (b as HTMLElement).dataset.val ?? "system" });
      applyTheme();
      void save();
      render();
    });
  });
  // Phase 44 — Kill switch global disable toggle
  void refreshKillSwitch();
  void refreshSafeMode();
  document.getElementById("ks-enable")?.addEventListener("click", () => void onEnableKillSwitch());
  document.getElementById("ks-disable")?.addEventListener("click", () => void onDisableKillSwitch());
}

function wireSoundUpload(inputId: string, resetId: string, kind: SoundKind): void {
  document.getElementById(inputId)?.addEventListener("change", (e) => {
    const file = (e.target as HTMLInputElement).files?.[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = () => setCustomSound(kind, String(reader.result ?? ""));
    reader.readAsDataURL(file);
  });
  document.getElementById(resetId)?.addEventListener("click", () => setCustomSound(kind, null));
}

async function refreshAgents(): Promise<void> {
  try {
    agents = await invoke<AgentInfo[]>("get_agents");
  } catch {
    agents = [];
  }
  const box = document.getElementById("agent-list");
  if (!box) return;
  box.innerHTML = agents
    .map(
      (a) =>
        `<div class="sess"><b>${esc(a.display_name)}</b><span class="msg">${esc(a.note ?? "")}</span><button data-kind="${esc(a.kind)}" type="button">${esc(a.installed ? t("remove") : t("install"))}</button></div>`,
    )
    .join("");
  box.querySelectorAll("button[data-kind]").forEach((b) => {
    b.addEventListener("click", async () => {
      await invoke("toggle_agent", { kind: (b as HTMLElement).dataset.kind });
      await refreshAgents();
    });
  });
}

// Phase 44 — Kill switch global disable toggle.
interface CliCommandStatus {
  supported: boolean;
  installed: boolean;
  foreign: boolean;
  target: string;
  shim: string;
}

/// Settings → General: the global `opencapx` command — a symlink into PATH installed on demand.
async function refreshCliCommand(): Promise<void> {
  const status = document.getElementById("cli-status");
  const btn = document.getElementById("cli-toggle") as HTMLButtonElement | null;
  if (!status || !btn) return;
  try {
    const s = await invoke<CliCommandStatus>("cli_command_status");
    if (!s.supported) {
      status.className = "ks-status ks-off";
      status.textContent = t("cliCommandUnsupported");
      btn.style.display = "none";
      return;
    }
    btn.style.display = "";
    btn.dataset.mode = s.installed ? "uninstall" : "install";
    btn.textContent = s.installed ? t("cliCommandUninstall") : t("cliCommandInstall");
    status.className = `ks-status ${s.installed ? "ks-on" : "ks-off"}`;
    status.textContent = s.installed ? `${t("cliCommandInstalled")} ${s.target}` : t("cliCommandNotInstalled");
  } catch (err) {
    status.className = "ks-status ks-err";
    status.textContent = `✗ ${String(err)}`;
  }
}

interface KillSwitchState {
  enabled: boolean;
  reason: string;
  setAt: number;
  setBy: string;
}

async function refreshKillSwitch(): Promise<void> {
  const status = document.getElementById("ks-status");
  if (!status) return;
  try {
    const s = await invoke<KillSwitchState>("get_kill_switch_state");
    if (s.enabled) {
      const sinceDate = new Date(s.setAt * 1000).toLocaleString();
      status.className = "ks-status ks-on";
      status.innerHTML = `${esc(t("killSwitchActive"))} · ${esc(sinceDate)}${s.reason ? " · " + esc(s.reason) : ""}`;
    } else {
      status.className = "ks-status ks-off";
      status.textContent = t("killSwitchInactive");
    }
  } catch (err) {
    status.className = "ks-status ks-err";
    status.textContent = `✗ ${String(err)}`;
  }
}

/// --safe-mode read-only state: decided at core startup, cannot be toggled while running (only a restart exits it).
async function refreshSafeMode(): Promise<void> {
  const status = document.getElementById("sm-status");
  if (!status) return;
  try {
    const active = await invoke<boolean>("get_safe_mode_state");
    status.className = `ks-status ${active ? "ks-on" : "ks-off"}`;
    status.textContent = active ? t("safeModeActive") : t("safeModeInactive");
  } catch (err) {
    status.className = "ks-status ks-err";
    status.textContent = `✗ ${String(err)}`;
  }
}

async function onEnableKillSwitch(): Promise<void> {
  const reasonEl = document.getElementById("ks-reason") as HTMLInputElement | null;
  const reason = reasonEl?.value.trim() ?? "";
  if (!window.confirm(t("killSwitchConfirmEnable"))) return;
  try {
    await invoke<KillSwitchState>("enable_kill_switch", { reason: reason || null });
    await refreshKillSwitch();
    void refreshPlugins();
  } catch (err) {
    const status = document.getElementById("ks-status");
    if (status) {
      status.className = "ks-status ks-err";
      status.textContent = `✗ ${String(err)}`;
    }
  }
}

async function onDisableKillSwitch(): Promise<void> {
  if (!window.confirm(t("killSwitchConfirmDisable"))) return;
  try {
    await invoke<KillSwitchState>("disable_kill_switch");
    await refreshKillSwitch();
  } catch (err) {
    const status = document.getElementById("ks-status");
    if (status) {
      status.className = "ks-status ks-err";
      status.textContent = `✗ ${String(err)}`;
    }
  }
}
