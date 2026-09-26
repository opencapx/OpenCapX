import { t } from "../../i18n";

export function setAlertingMsg(text: string): void {
  const m = document.getElementById("alerting-msg");
  if (m) m.textContent = text;
}

// Phase 60: frontend builtin list (one-to-one with backend builtin_presets(); lists only kind + name for the fork prompt)
export const FRONTEND_BUILTIN_PRESETS: { kind: string; name: string }[] = [
  { kind: "builtin:slack",        name: "Slack incoming webhook" },
  { kind: "builtin:discord",      name: "Discord webhook" },
  { kind: "builtin:msteams",      name: "Microsoft Teams webhook" },
  { kind: "builtin:generic_json", name: "Generic JSON envelope" },
  { kind: "builtin:plain_text",   name: "Plain text" },
];

export const DEFAULT_TEMPLATE_SAMPLE = JSON.stringify(
  { user: "alice", count: 42, items: [1, 2, 3], nested: { ok: true } },
  null,
  2,
);

export const ALERTING_SOURCES = [
  { id: "plugin.metrics.exceeded", i18n: "alertingSourceMetrics" },
  { id: "capability.sla.violated", i18n: "alertingSourceSla" },
  { id: "plugin.kill_switch.enabled", i18n: "alertingSourceKillSwitch" },
  { id: "plugin.lifecycle.crashed", i18n: "alertingSourceCrash" },
] as const;

// ─── Phase 50: silences + acks helpers ────────────────────────────────────────

function weekdayLabel(bit: number): string {
  const names = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
  return names[bit] || "?";
}

export function weekdayBitsToLabels(weekdays: number): string {
  const labels: string[] = [];
  for (let i = 0; i < 7; i++) {
    if (weekdays & (1 << i)) labels.push(weekdayLabel(i));
  }
  return labels.length === 7 ? t("alertingEveryday") : labels.join(", ");
}

export function formatUnix(ts: number): string {
  const d = new Date(ts * 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())} ${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())} UTC`;
}
