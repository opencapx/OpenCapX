import { invoke } from "@tauri-apps/api/core";
import { connectEventStream, onEvent } from "../events";
import { t } from "../i18n";
import { esc } from "./shared";

// ---- SLA tab (Phase 36 — Capability SLA monitoring / alert thresholds) --------------------

interface SlaConfig {
  p95ThresholdMs: number;
  failRateThresholdPct: number;
  windowSecs: number;
  pollSecs: number;
}

interface SlaViolation {
  pluginId: string;
  capability: string;
  kind: string;
  threshold: number;
  observed: number;
  sinceTs: number;
  samples: number;
}

let slaStreamOnEvent: (() => void) | null = null;

async function refreshSla(): Promise<void> {
  const cfgBox = document.getElementById("sla-config");
  const listBox = document.getElementById("sla-list");
  const msg = document.getElementById("sla-msg");
  const dot = document.getElementById("sla-dot");
  if (!cfgBox || !listBox) return;
  try {
    const cfg = await invoke<SlaConfig>("get_sla_config");
    cfgBox.innerHTML = `
      <div class="sla-grid">
        <label class="sla-field"><span class="sla-field-label">${esc(t("slaP95"))}</span><input type="number" id="sla-p95" min="0" step="50" value="${cfg.p95ThresholdMs}" /><span class="sla-field-hint">${esc(t("slaP95Hint"))}</span></label>
        <label class="sla-field"><span class="sla-field-label">${esc(t("slaFailRate"))}</span><input type="number" id="sla-failrate" min="0" max="100" step="1" value="${cfg.failRateThresholdPct}" /><span class="sla-field-hint">${esc(t("slaFailRateHint"))}</span></label>
        <label class="sla-field"><span class="sla-field-label">${esc(t("slaWindow"))}</span><input type="number" id="sla-window" min="1" max="86400" step="1" value="${cfg.windowSecs}" /><span class="sla-field-hint">${esc(t("slaWindowHint"))}</span></label>
        <label class="sla-field"><span class="sla-field-label">${esc(t("slaPoll"))}</span><input type="number" id="sla-poll" min="2" max="3600" step="1" value="${cfg.pollSecs}" /><span class="sla-field-hint">${esc(t("slaPollHint"))}</span></label>
      </div>
      <div class="sla-actions"><button class="btn primary" id="sla-save" type="button">${esc(t("slaSave"))}</button></div>
    `;
    document.getElementById("sla-save")?.addEventListener("click", () => void saveSla());

    const violations = await invoke<SlaViolation[]>("list_sla_violations", { limit: 100 });
    // hero status dot: 0 alerts = green; only latency = amber; any failure = red
    const anyFailure = violations.some((v) => v.kind !== "latency");
    if (dot) dot.className = `sla-dot ${violations.length === 0 ? "ok" : anyFailure ? "err" : "warn"}`;
    // Streaming redraws don't replay the enter animation: .fresh-tab lingers until the next render(),
    // so after the first frame we add no-enter to the list to turn it off (same approach as .metrics-no-enter).
    const painted = listBox.dataset.slaPainted === "1";
    listBox.classList.toggle("sla-no-enter", painted);
    listBox.dataset.slaPainted = "1";
    if (violations.length === 0) {
      listBox.innerHTML = `<div class="sla-ok">${esc(t("slaNoViolations"))}</div>`;
    } else {
      listBox.innerHTML = violations
        .map((v) => {
          const ts = new Date(v.sinceTs * 1000).toISOString().replace("T", " ").slice(0, 19);
          return slaRowHtml(v, ts);
        })
        .join("");
    }
    if (msg) msg.textContent = `${violations.length}`;
  } catch (e) {
    if (dot) dot.className = "sla-dot err";
    if (msg) msg.textContent = `✗ ${String(e)}`;
    listBox.innerHTML = "";
  }
}

async function saveSla(): Promise<void> {
  const msg = document.getElementById("sla-msg");
  const cfg = {
    p95ThresholdMs: Number((document.getElementById("sla-p95") as HTMLInputElement | null)?.value ?? 0),
    failRateThresholdPct: Number((document.getElementById("sla-failrate") as HTMLInputElement | null)?.value ?? 0),
    windowSecs: Number((document.getElementById("sla-window") as HTMLInputElement | null)?.value ?? 0),
    pollSecs: Number((document.getElementById("sla-poll") as HTMLInputElement | null)?.value ?? 0),
  };
  try {
    await invoke("set_sla_config", { cfg });
    if (msg) msg.textContent = t("slaSaved");
    void refreshSla();
  } catch (e) {
    if (msg) msg.textContent = `✗ ${String(e)}`;
  }
}

/// Row HTML: shared between the list's first paint and stream prepends; both places must stay structurally identical.
function slaRowHtml(v: SlaViolation, tsStr: string): string {
  const isLatency = v.kind === "latency";
  const obsStr = isLatency ? `${v.observed.toFixed(0)} ms` : `${v.observed.toFixed(1)}%`;
  const thrStr = isLatency ? `${v.threshold.toFixed(0)} ms` : `${v.threshold.toFixed(1)}%`;
  const cls = isLatency ? "sla-latency" : "sla-failure";
  return `<div class="sess sla-row ${cls}"><b>${esc(v.pluginId)}</b><span class="msg"><code>${esc(v.capability)}</code> · <span class="sla-kind">${esc(t(isLatency ? "slaKindLatency" : "slaKindFailure"))}</span> · observed <b>${esc(obsStr)}</b> &gt; threshold ${esc(thrStr)} · ${v.samples} ${esc(t("slaSamples"))}</span><span class="sla-ts">${esc(tsStr)}</span></div>`;
}

function startSlaStream(): void {
  stopSlaStream();
  connectEventStream();
  slaStreamOnEvent = onEvent("capability.sla.violated", (ev) => {
    const listBox = document.getElementById("sla-list");
    if (!listBox) return;
    const ok = listBox.querySelector(".sla-ok");
    if (ok) listBox.innerHTML = "";
    // Rows inserted out of order are not the tab-switch first frame: explicitly turn off the enter animation so a lingering .fresh-tab doesn't replay it
    listBox.classList.add("sla-no-enter");
    const p = (ev.payload ?? {}) as Record<string, unknown>;
    const ts = new Date(ev.timestamp * 1000).toISOString().replace("T", " ").slice(0, 19);
    listBox.insertAdjacentHTML("afterbegin", slaRowHtml({
      pluginId: String(p.pluginId ?? "?"),
      capability: String(p.capability ?? "?"),
      kind: String(p.kind ?? "?"),
      threshold: Number(p.threshold ?? 0),
      observed: Number(p.observed ?? 0),
      sinceTs: ev.timestamp,
      samples: Number(p.samples ?? 0),
    }, ts));
  });
}

export function stopSlaStream(): void {
  slaStreamOnEvent?.();
  slaStreamOnEvent = null;
}

export function renderSla(body: HTMLElement): void {
  // hero (status dot + count + refresh) + threshold card + alert card: the same depth language as the metrics/plugin settings pages,
  // no settings-list wrapper (cards carry their own ring shadow; nesting looks dirty).
  body.innerHTML = `<div class="sla-page">
    <div class="sla-hero"><span class="sla-dot" id="sla-dot" aria-hidden="true"></span><div class="sla-hero-info"><span class="sla-hero-title">${esc(t("tabSla"))}</span><span class="setting-hint" id="sla-msg"></span></div><button class="btn ghost sla-refresh-btn" id="sla-refresh" type="button">${esc(t("slaRefresh"))}</button></div>
    <div class="sla-thresh"><p class="sla-card-title">${esc(t("slaHint"))}</p><div id="sla-config"></div></div>
    <div class="sla-list-card"><p class="sla-card-title">${esc(t("slaViolationsHint"))}</p><div id="sla-list"></div></div>
  </div>`;
  document.getElementById("sla-refresh")?.addEventListener("click", () => void refreshSla());
  void refreshSla();
  startSlaStream();
}
