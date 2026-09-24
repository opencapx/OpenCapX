import "./settings.css";
import { t } from "./i18n";
import { ICON_ABOUT, ICON_AGENTS, ICON_AUDIT, ICON_AUTOMATION, ICON_BUBBLE, ICON_CONFIG, ICON_GENERAL, ICON_LOGS, ICON_NOTIFY, ICON_PET, ICON_PLUGINS, ICON_SLA, ICON_STATS } from "./icons";
import { getVersion } from "@tauri-apps/api/app";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  disconnectEventStream,
} from "./events";
import {
  esc,
  getAppVersion,
  load,
  loadDbRecoveryNotice,
  refreshPluginConfig,
  refreshSessions,
  registerLegacyCleanup,
  registerLegacyRenderer,
  registerTab,
  render,
  setAppVersion,
  setWindowFocused,
  startThemeListener,
} from "./settings/shared";
import { renderAgents } from "./settings/agents";
import { renderAlerting } from "./settings/alerting";
import { renderAudit, stopAuditStream } from "./settings/audit";
import { renderAutomation } from "./settings/automation";
import { renderBubbleSettings } from "./settings/bubble";
import { renderBackup } from "./settings/backup";
import { renderCapabilities } from "./settings/capabilities";
import { renderHotkeys } from "./settings/hotkeys";
import { renderLogs, stopLogsStream } from "./settings/logs";
import { renderMarket } from "./settings/market";
import { renderMetricsTab, stopMetricsStream } from "./settings/metrics";
import { renderNotify } from "./settings/notify";
import { renderProfilesTab, stopWorkspaceListener } from "./settings/profiles";
import { renderPlugins } from "./settings/plugins";
import { renderGeneral } from "./settings/general";
import { renderPet } from "./settings/pet";
import { renderPluginPageTab } from "./settings/plugin-page";
import { renderRules } from "./settings/rules";
import { renderStats } from "./settings/stats";
import { renderSla } from "./settings/sla";
import { renderRpcTrace } from "./settings/rpc-trace";
import { startInstallAskListener } from "./settings/modals";

function renderLegacyTab(body: HTMLElement): void {
    // Fallback: an unknown tab doesn't white-screen, it only shows the version card.
    body.innerHTML =
      `<div class="settings-list"><div class="about-card"><div class="logo">${ICON_PET}</div><div><b>OpenCapX</b></div><div class="ver">${esc(t("version"))} ${esc(getAppVersion())}</div><p>${esc(t("aboutText"))}</p></div></div>`;
}

startThemeListener();

void getCurrentWindow()
  .onFocusChanged(({ payload: focused }) => {
    setWindowFocused(focused);
  })
  .catch(() => undefined);

registerLegacyCleanup(() => {
  stopWorkspaceListener();
});
registerLegacyRenderer(renderLegacyTab);
registerTab({ id: "general", render: renderGeneral });
registerTab({ id: "pet", render: renderPet });
registerTab({ id: "agents", render: renderAgents });
registerTab({ id: "automation", render: renderAutomation });
registerTab({ id: "bubble", render: renderBubbleSettings });
registerTab({ id: "rules", render: renderRules });
registerTab({ id: "backup", render: renderBackup });
registerTab({ id: "hotkeys", render: renderHotkeys });
registerTab({ id: "logs", render: renderLogs, stop: stopLogsStream });
registerTab({ id: "market", render: renderMarket });
registerTab({ id: "plugins", render: renderPlugins });
registerTab({ id: "plugin:", render: renderPluginPageTab });
registerTab({ id: "metrics", render: renderMetricsTab, stop: stopMetricsStream });
registerTab({ id: "alerting", render: renderAlerting });
registerTab({ id: "notify", render: renderNotify });
registerTab({ id: "profiles", render: renderProfilesTab });
registerTab({ id: "stats", render: renderStats });
registerTab({ id: "sla", render: renderSla });
registerTab({ id: "rpcTrace", render: renderRpcTrace });
registerTab({ id: "capabilities", render: renderCapabilities });
registerTab({ id: "audit", render: renderAudit, stop: stopAuditStream });

void load()
  .then(loadDbRecoveryNotice)
  .then(() => getVersion().then((v) => { setAppVersion(v); }).catch(() => undefined))
  .then(() => {
    startInstallAskListener();
    render();
    void refreshSessions();
    // Fetch once at startup for the sidebar 'Plugin Settings' section: opening settings already has the entry, without clicking another tab first;
    // catch fallback ensures no failure in the startup chain becomes an unhandled rejection.
    void refreshPluginConfig().catch(() => undefined);
    window.setInterval(() => void refreshSessions(), 2000);
  });

