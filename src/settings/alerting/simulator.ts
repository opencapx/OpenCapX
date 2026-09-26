import { invoke } from "@tauri-apps/api/core";
import { t } from "../../i18n";
import { esc } from "../shared";
import { EndpointPreviewRowDto, renderPreviewRow } from "./endpoints";
import { EscalationRuleDto } from "./escalations";
import { RouteRuleDto } from "./routes";

// Phase 75 — Alerting full-path dry-run DTO
interface RouteSimulationDto {
  matchedRule: RouteRuleDto | null;
  targetEndpointIds: string[];
  fanoutAll: boolean;
}

interface AggregationSimulationDto {
  matchedRuleId: string | null;
  matchedRuleName: string | null;
  bucketEventsInWindow: number;
  threshold: number;
  wouldFire: boolean;
  action: string | null;
  actionSeverity: string | null;
}

interface CorrelationSimulationDto {
  matchedARuleIds: string[];
  matchedBRuleIds: string[];
  wouldSuppress: boolean;
  suppressingRuleId: string | null;
  propagatedSeverity: string | null;
}

interface EscalationSimulationDto {
  lastDispatchAt: number | null;
  candidateRules: EscalationRuleDto[];
  wouldEscalate: boolean;
  targetSeverity: string | null;
}

// Phase 76 — hit details of the three gates (silence / ack / dedup)
interface SilenceHitDto {
  id: string;
  name: string;
  kindPattern: string;
  endsAt: number;
  remainingSecs: number;
}

interface AckHitDto {
  id: string;
  kindPattern: string;
  ackUntil: number;
  remainingSecs: number;
}

interface DedupHitDto {
  key: string;
  lastSentSecsAgo: number;
  minIntervalSecs: number;
  remainingSecs: number;
}

interface DispatchGateSimulationDto {
  silenced: SilenceHitDto | null;
  acked: AckHitDto | null;
  dedupBlocked: DedupHitDto | null;
  wouldBeDropped: boolean;
}

interface AlertingDispatchSimulationDto {
  source: string;
  payloadSummary: string;
  // Phase 76 — gates run before all links (silence/ack/dedup short-circuit at the start of dispatch)
  gates: DispatchGateSimulationDto;
  route: RouteSimulationDto;
  correlation: CorrelationSimulationDto;
  aggregation: AggregationSimulationDto;
  escalation: EscalationSimulationDto;
  endpoints: EndpointPreviewRowDto[];
}

// ─── Phase 75: Alerting full-path dry-run simulator ─────────────────────────────

export async function onSimulateDispatch(): Promise<void> {
  const sourceEl = document.getElementById("alerting-sim-source") as HTMLInputElement | null;
  const payloadEl = document.getElementById("alerting-sim-payload") as HTMLTextAreaElement | null;
  const result = document.getElementById("alerting-sim-result");
  const msg = document.getElementById("alerting-sim-msg");
  const source = sourceEl?.value.trim() ?? "";
  if (!source) {
    if (msg) msg.textContent = `✗ ${t("alertingDispatchSimulateSourceRequired")}`;
    return;
  }
  const payload = payloadEl?.value ?? "";
  if (msg) msg.textContent = t("alertingDispatchSimulateRunning");
  if (result) result.innerHTML = "";
  try {
    const sim = await invoke<AlertingDispatchSimulationDto>("simulate_alerting_dispatch", {
      source,
      payload,
    });
    if (msg) msg.textContent = "";
    if (result) result.innerHTML = renderDispatchSimulation(sim);
  } catch (err) {
    if (msg) msg.textContent = `✗ ${String(err)}`;
    if (result) result.innerHTML = `<div class="alerting-sim-error">${esc(String(err))}</div>`;
  }
}

function renderDispatchSimulation(sim: AlertingDispatchSimulationDto): string {
  return [
    renderSimGates(sim.gates),
    renderSimRoute(sim.route),
    renderSimAggregation(sim.aggregation),
    renderSimCorrelation(sim.correlation),
    renderSimEscalation(sim.escalation),
    renderSimEndpoints(sim.endpoints),
  ].join("");
}

// Phase 76 — gates short-circuit at the start of dispatch (silence -> ack -> dedup).
function renderSimGates(g: DispatchGateSimulationDto): string {
  const silencePill = g.silenced
    ? `<span class="alerting-sim-pill silenced">🤫 ${esc(t("alertingDispatchSimulateSilenced"))} <code>${esc(g.silenced.name || g.silenced.kindPattern)}</code> <span class="setting-hint">${esc(t("alertingDispatchSimulateRemaining"))}: ${g.silenced.remainingSecs}s</span></span>`
    : `<span class="alerting-sim-pill pass">${esc(t("alertingDispatchSimulateNotSilenced"))}</span>`;
  const ackPill = g.acked
    ? `<span class="alerting-sim-pill acked">✔ ${esc(t("alertingDispatchSimulateAcked"))} <code>${esc(g.acked.kindPattern)}</code> <span class="setting-hint">${esc(t("alertingDispatchSimulateRemaining"))}: ${g.acked.remainingSecs}s</span></span>`
    : `<span class="alerting-sim-pill pass">${esc(t("alertingDispatchSimulateNotAcked"))}</span>`;
  const dedupPill = g.dedupBlocked
    ? `<span class="alerting-sim-pill dedup">⏳ ${esc(t("alertingDispatchSimulateDedupBlocked"))} <span class="setting-hint">${esc(t("alertingDispatchSimulateRemaining"))}: ${g.dedupBlocked.remainingSecs}s</span></span>`
    : `<span class="alerting-sim-pill pass">${esc(t("alertingDispatchSimulateNotDedup"))}</span>`;
  const droppedBanner = g.wouldBeDropped
    ? `<div class="alerting-sim-banner">${esc(t("alertingDispatchSimulateWouldBeDropped"))}</div>`
    : "";
  return `<div class="alerting-sim-block">
    <h4>${esc(t("alertingDispatchSimulateGatesLink"))}</h4>
    <div class="alerting-sim-gate-row">${silencePill}</div>
    <div class="alerting-sim-gate-row">${ackPill}</div>
    <div class="alerting-sim-gate-row">${dedupPill}</div>
    ${droppedBanner}
  </div>`;
}

function renderSimRoute(r: RouteSimulationDto): string {
  const pill = r.matchedRule
    ? `<span class="alerting-sim-pill routing">→ ${esc(r.matchedRule.name)}</span>`
    : `<span class="alerting-sim-pill fanout">${esc(t("alertingDispatchSimulateFanoutAll"))}</span>`;
  const targets = r.matchedRule
    ? `<div class="alerting-sim-targets">${esc(t("alertingDispatchSimulateTargets"))}: ${r.targetEndpointIds.map((id) => esc(id)).join(", ") || "—"}</div>`
    : "";
  return `<div class="alerting-sim-block">
    <h4>${esc(t("alertingDispatchSimulateRouteLink"))}</h4>
    ${pill}
    ${targets}
  </div>`;
}

function renderSimAggregation(a: AggregationSimulationDto): string {
  if (!a.matchedRuleId) {
    return `<div class="alerting-sim-block">
      <h4>${esc(t("alertingDispatchSimulateAggregationLink"))}</h4>
      <span class="alerting-sim-pill pass">${esc(t("alertingDispatchSimulateNoRule"))}</span>
    </div>`;
  }
  const pill = a.wouldFire
    ? `<span class="alerting-sim-pill fire">🔥 ${esc(a.action ?? "?")}${a.actionSeverity ? ` → ${esc(a.actionSeverity)}` : ""}</span>`
    : `<span class="alerting-sim-pill pass">${esc(t("alertingDispatchSimulateWouldNotFire"))}</span>`;
  return `<div class="alerting-sim-block">
    <h4>${esc(t("alertingDispatchSimulateAggregationLink"))}</h4>
    <div class="alerting-sim-rule">${esc(a.matchedRuleName ?? a.matchedRuleId)}</div>
    <div class="alerting-sim-bucket">${esc(t("alertingDispatchSimulateBucket"))}: <code>${a.bucketEventsInWindow} / ${a.threshold}</code></div>
    ${pill}
  </div>`;
}

function renderSimCorrelation(c: CorrelationSimulationDto): string {
  const pill = c.wouldSuppress
    ? `<span class="alerting-sim-pill suppress">⛔ ${esc(t("alertingDispatchSimulateWouldSuppress"))}${c.suppressingRuleId ? ` <code>${esc(c.suppressingRuleId)}</code>` : ""}</span>`
    : `<span class="alerting-sim-pill pass">${esc(t("alertingDispatchSimulateNoSuppress"))}</span>`;
  return `<div class="alerting-sim-block">
    <h4>${esc(t("alertingDispatchSimulateCorrelationLink"))}</h4>
    <div class="alerting-sim-pair">
      <span class="alerting-sim-pair-label">A</span>: ${c.matchedARuleIds.map((id) => esc(id)).join(", ") || "—"}<br />
      <span class="alerting-sim-pair-label">B</span>: ${c.matchedBRuleIds.map((id) => esc(id)).join(", ") || "—"}
    </div>
    ${pill}
  </div>`;
}

function renderSimEscalation(e: EscalationSimulationDto): string {
  const pill = e.wouldEscalate
    ? `<span class="alerting-sim-pill escalate">⬆ ${esc(t("alertingDispatchSimulateWouldEscalate"))} → <span class="severity-badge sev-${esc(e.targetSeverity ?? "?")}">${esc(e.targetSeverity ?? "?")}</span></span>`
    : `<span class="alerting-sim-pill pass">${esc(t("alertingDispatchSimulateNoEscalate"))}</span>`;
  return `<div class="alerting-sim-block">
    <h4>${esc(t("alertingDispatchSimulateEscalationLink"))}</h4>
    <div class="alerting-sim-last">${esc(t("alertingDispatchSimulateLastDispatch"))}: ${e.lastDispatchAt ?? "—"}</div>
    ${pill}
  </div>`;
}

function renderSimEndpoints(eps: EndpointPreviewRowDto[]): string {
  if (eps.length === 0) {
    return `<div class="alerting-sim-block">
      <h4>${esc(t("alertingDispatchSimulateEndpointsLink"))}</h4>
      <div class="alerting-sim-empty">${esc(t("alertingDispatchSimulateNoEndpoints"))}</div>
    </div>`;
  }
  return `<div class="alerting-sim-block">
    <h4>${esc(t("alertingDispatchSimulateEndpointsLink"))}</h4>
    ${eps.map((row) => renderPreviewRow(row)).join("")}
  </div>`;
}
