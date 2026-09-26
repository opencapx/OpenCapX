import { invoke } from "@tauri-apps/api/core";
import { t } from "../../i18n";
import { esc } from "../shared";
import { CycleReportDto, RouteSeenEventDto } from "./routes";

// ─── Phase 68: Correlation — cycle linter + timeline ────────────────────

function cycleKindLabel(kind: CycleReportDto["kind"]): string {
  if (kind === "self_loop") return t("alertingCorrelationCycleKindSelf");
  if (kind === "route_to_route") return t("alertingCorrelationCycleKindRoute");
  return t("alertingCorrelationCycleKindCorrelation");
}

export async function onDetectCycles(): Promise<void> {
  const msg = document.getElementById("alerting-correlation-msg");
  const out = document.getElementById("alerting-correlation-cycles-result");
  if (!out) return;
  try {
    const reports = await invoke<CycleReportDto[]>("detect_alerting_cycles");
    if (reports.length === 0) {
      out.innerHTML = `<span class="setting-hint">${esc(t("alertingCorrelationNoCycle"))}</span>`;
      if (msg) msg.textContent = "";
      return;
    }
    out.innerHTML = reports.map((r) => {
      const arrowed = r.cycle.map((n, i) => `${i > 0 ? " → " : ""}<code>${esc(n)}</code>`).join("");
      return `<div class="alerting-correlation-cycle"><strong>[${esc(cycleKindLabel(r.kind))}]</strong> ${arrowed}</div>`;
    }).join("");
    if (msg) msg.textContent = `${reports.length} cycle(s)`;
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

export async function onRefreshTimeline(): Promise<void> {
  const msg = document.getElementById("alerting-correlation-msg");
  const tl = document.getElementById("alerting-correlation-timeline");
  if (!tl) return;
  try {
    const events = await invoke<RouteSeenEventDto[]>("recent_alerting_events", { limit: 50 });
    if (events.length === 0) {
      tl.innerHTML = `<span class="setting-hint">${esc(t("alertingCorrelationTimelineEmpty"))}</span>`;
      if (msg) msg.textContent = "";
      return;
    }
    const now = Math.floor(Date.now() / 1000);
    tl.innerHTML = events.map((e) => {
      const ago = now - e.tsSecs;
      const ts = ago < 5 ? t("alertingCorrelationTimelineNow") : `${ago}${t("alertingCorrelationTimelineSecsAgo")}`;
      const fired = e.routesFired.length > 0
        ? ` <span class="routes-fired">${esc(t("alertingCorrelationTimelineRoutesFired"))} ${e.routesFired.map(esc).join(", ")}</span>`
        : "";
      const corr = e.correlationsHit.length > 0
        ? ` <span class="corr-hit">[${esc(t("alertingCorrelationTimelineCorrelationsHit"))} ${e.correlationsHit.map(esc).join(", ")}]</span>`
        : "";
      return `<div class="alerting-correlation-timeline-event"><span class="ts">${esc(ts)}</span> <code>${esc(e.source)}</code> <span class="payload-summary">${esc(e.payloadSummary)}</span>${fired}${corr}</div>`;
    }).join("");
    if (msg) msg.textContent = `${events.length} event(s)`;
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}
