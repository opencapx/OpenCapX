import { invoke } from "@tauri-apps/api/core";
import { t } from "../i18n";
import {
  dayLabel,
  detailBlock,
  detailKv,
  esc,
  getNotifUnread,
  getSettings,
  group,
  setNotifUnread,
  setSettings,
} from "./shared";

// ─── i2 §13 Notification Center ──────────────────────────────────────────────

export function renderNotify(body: HTMLElement): void {
  // i2 §13 Notification Center: a digest of all Agents' notifications (see nav for the notifUnread badge)
  body.innerHTML = group("tabNotify",
    `<div class="setting-row vertical"><span class="setting-hint">${esc(t("notifHint"))}</span><div class="audit-search"><input type="text" id="notif-agent" placeholder="${esc(t("notifAgentPlaceholder"))}" /><button class="btn ghost" id="notif-refresh" type="button">${esc(t("auditRefresh"))}</button><button class="btn ghost" id="notif-read-all" type="button">${esc(t("notifMarkRead"))}</button><span class="setting-hint" id="notif-msg"></span></div><div id="notif-list"></div></div>`);
  document.getElementById("notif-refresh")?.addEventListener("click", () => void refreshNotifications());
  document.getElementById("notif-read-all")?.addEventListener("click", () => void markNotificationsRead());
  document.getElementById("notif-agent")?.addEventListener("keydown", (ev) => {
    if ((ev as KeyboardEvent).key === "Enter") void refreshNotifications();
  });
  void refreshNotifications();
}

interface NotificationEntry {
  id: string;
  timestamp: number;
  agent: string;
  title: string;
  body: string;
  severity: string;
}

const NOTIFY_ICONS: Record<string, string> = {
  info: "✓",
  warn: "⚠",
  error: "✗",
};

/// Notification record: collapsed it shows only 'time · level icon · Agent · title'; expand to see body/level/ID.
function renderNotificationRow(e: NotificationEntry): string {
  const time = new Date(e.timestamp * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  const fullTs = new Date(e.timestamp * 1000).toISOString().replace("T", " ").slice(0, 19);
  const icon = NOTIFY_ICONS[e.severity] ?? NOTIFY_ICONS.info;
  const detail =
    detailBlock(t("recBody"), e.body) +
    detailKv(t("recSeverity"), e.severity) +
    detailKv(t("auditDetailId"), e.id) +
    detailKv(t("auditDetailTime"), fullTs);
  const unread = e.timestamp > (getSettings().notifLastRead ?? 0);
  return `<details class="rec${unread ? " notif-unread" : ""}"><summary class="rec-head"><span class="rec-time">${esc(time)}</span><span class="rec-icon">${icon}</span><span class="rec-text"><b>${esc(e.agent || "—")}</b> ${esc(e.title)}</span></summary><div class="rec-detail">${detail}</div></details>`;
}

/// Only updates the sidebar unread badge. Must not call render() here: render()'s notify branch would again
/// call refreshNotifications, forming an infinite render -> refresh -> render loop.
function updateNotifBadge(): void {
  const nav = document.querySelector<HTMLElement>('button[data-tab="notify"]');
  if (!nav) return;
  let badge = nav.querySelector<HTMLElement>(".nav-badge");
  if (getNotifUnread() <= 0) {
    badge?.remove();
    return;
  }
  if (!badge) {
    badge = document.createElement("span");
    badge.className = "nav-badge";
    const anchor = nav.querySelector(".active-indicator");
    if (anchor) nav.insertBefore(badge, anchor);
    else nav.appendChild(badge);
  }
  badge.textContent = getNotifUnread() > 99 ? "99+" : String(getNotifUnread());
}

async function refreshNotifications(): Promise<void> {
  const box = document.getElementById("notif-list");
  const msg = document.getElementById("notif-msg");
  if (!box) return;
  try {
    const agent = (document.getElementById("notif-agent") as HTMLInputElement | null)?.value.trim() ?? "";
    const entries = await invoke<NotificationEntry[]>("list_notifications", { limit: 300, agent: agent || null });
    // Unread = after notifLastRead (not filtered by agent — the badge count covers all notifications)
    if (!agent) {
      setNotifUnread(entries.filter((e) => e.timestamp > (getSettings().notifLastRead ?? 0)).length);
      updateNotifBadge();
    }
    if (entries.length === 0) {
      box.innerHTML = `<div class="setting-hint">${esc(t("notifEmpty"))}</div>`;
      if (msg) msg.textContent = "";
      return;
    }
    // Group by day (same as Timeline)
    const groups: { day: string; items: NotificationEntry[] }[] = [];
    for (const e of entries) {
      const day = dayLabel(e.timestamp);
      const last = groups[groups.length - 1];
      if (last && last.day === day) last.items.push(e);
      else groups.push({ day, items: [e] });
    }
    box.innerHTML = groups
      .map((g) => `<div class="tl-day">${esc(g.day)}</div>` + g.items.map(renderNotificationRow).join(""))
      .join("");
    if (msg) msg.textContent = `${entries.length}`;
  } catch (e) {
    if (msg) msg.textContent = `✗ ${String(e)}`;
    box.innerHTML = "";
  }
}

/// 'Mark all read': bump notifLastRead to now and persist it to settings.json.
async function markNotificationsRead(): Promise<void> {
  setSettings({ ...getSettings(), notifLastRead: Math.floor(Date.now() / 1000) });
  setNotifUnread(0);
  await invoke("set_settings", { value: getSettings() });
  updateNotifBadge();
  await refreshNotifications();
}
