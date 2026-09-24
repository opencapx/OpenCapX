import { invoke } from "@tauri-apps/api/core";
import { onEvent } from "../events";
import { t } from "../i18n";
import { esc } from "./shared";

export function renderMetricsTab(body: HTMLElement): void {
  // hero (status dot + count + refresh) + card grid + threshold card: the same depth language as the plugin settings page,
  // no settings-list wrapper (cards carry their own ring shadow; nesting looks dirty).
  body.innerHTML = `<div class="metrics-page">
    <div class="metrics-hero"><span class="metrics-dot ok" id="metrics-dot" aria-hidden="true"></span><div class="metrics-hero-info"><span class="metrics-hero-title">${esc(t("tabMetrics"))}</span><span class="setting-hint" id="metrics-msg"></span></div><button class="btn ghost metrics-refresh-btn" id="metrics-refresh" type="button">${esc(t("metricsRefresh"))}</button></div>
    <span class="setting-hint metrics-hint">${esc(t("metricsHint"))}</span>
    <div class="metrics-overview"><div class="metrics-overview-wrap"><canvas id="metrics-overview-canvas" class="metrics-overview-canvas" width="640" height="180" aria-hidden="true"></canvas></div><div class="metrics-overview-legend" id="metrics-overview-legend"></div></div>
    <div id="metrics-grid" class="metrics-grid"></div>
    <div class="metrics-thresh"><p class="metrics-thresh-title">${esc(t("metricsThresholdsHint"))}</p><div id="metrics-config"></div></div>
  </div>`;
  document.getElementById("metrics-refresh")?.addEventListener("click", () => void refreshMetrics());
  // Persist thresholds before fetching the snapshot: the first-frame card already has usage bars and alert rings (otherwise it waits for the next sampling cycle)
  void refreshMetricsConfig().then(() => refreshMetrics());
  startMetricsStream();
}

// ─── Phase 45: Plugin runtime metrics ─────────────────────────────────────────

interface PluginMetricsSnapshot {
  pluginId: string;
  pid: number;
  cpuPct: number | null;
  rssBytes: number | null;
  threads: number | null;
  fds: number | null;
  ts: number;
}
interface MetricsConfigDto {
  pollSecs: number;
  cpuPctMax: number;
  rssBytesMax: number;
  threadCountMax: number;
  keepSamples: number;
}

let metricsUnlistenSampled: (() => void) | null = null;
let metricsUnlistenExceeded: (() => void) | null = null;
let metricsCache: Record<string, PluginMetricsSnapshot> = {};
let metricsConfig: MetricsConfigDto | null = null;

// ── Chart data ring ────────────────────────────────────────────────────────────────
interface MetricsHistoryPoint {
  cpu: number | null;
  ts: number;
}
let metricsHistory: Record<string, MetricsHistoryPoint[]> = {};
const METRICS_HISTORY_CAP = 200;
// The sparkline shows 120 points and the overview 200: slices of the same cache, no extra requests
const METRICS_SPARK_LIMIT = 120;
// Sampling may be denser than 2s: the overview is drawn only once per streaming redraw window
const METRICS_OVERVIEW_THROTTLE_MS = 2000;
const METRICS_SERIES_COLORS = ["#60a5fa", "#4ade80", "#fbbf24", "#f472b6", "#a78bfa", "#22d3ee"];
let metricsOverviewDrawnAt = 0;

function pushHistory(pluginId: string, cpuPct: number | null, ts: number = Date.now()): void {
  const ring = metricsHistory[pluginId] ?? (metricsHistory[pluginId] = []);
  ring.push({ cpu: cpuPct, ts });
  if (ring.length > METRICS_HISTORY_CAP) ring.splice(0, ring.length - METRICS_HISTORY_CAP);
}

function fmtBytes(b: number | null): string {
  if (b === null || b === undefined) return "—";
  if (b < 1024) return `${b} B`;
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`;
  if (b < 1024 * 1024 * 1024) return `${(b / (1024 * 1024)).toFixed(1)} MB`;
  return `${(b / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

function metricStatus(cpu: number | null, rss: number | null, threads: number | null): "ok" | "warn" | "err" {
  if (!metricsConfig) return "ok";
  let worst: "ok" | "warn" | "err" = "ok";
  const c = metricsConfig;
  if (c.cpuPctMax > 0 && cpu !== null) {
    if (cpu >= c.cpuPctMax) worst = "err";
    else if (cpu >= c.cpuPctMax * 0.8 && worst === "ok") worst = "warn";
  }
  if (c.rssBytesMax > 0 && rss !== null) {
    if (rss >= c.rssBytesMax) worst = "err";
    else if (rss >= c.rssBytesMax * 0.8 && worst === "ok") worst = "warn";
  }
  if (c.threadCountMax > 0 && threads !== null) {
    if (threads >= c.threadCountMax) worst = "err";
    else if (threads >= c.threadCountMax * 0.8 && worst === "ok") worst = "warn";
  }
  return worst;
}

async function refreshMetrics(): Promise<void> {
  const grid = document.getElementById("metrics-grid");
  const msg = document.getElementById("metrics-msg");
  if (!grid) return;
  try {
    const snaps = await invoke<PluginMetricsSnapshot[]>("get_plugin_metrics");
    snaps.forEach((s) => { metricsCache[s.pluginId] = s; });
    // History snapshots are fetched only when opening the tab / manual refresh; streaming samples only append to the in-memory ring, no refetch
    await refreshMetricsHistories(snaps.map((s) => s.pluginId));
    renderMetricsGrid();
    drawMetricsOverview();
    if (msg) msg.textContent = `${snaps.length} ${esc(t("metricsPlugins"))}`;
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

function renderMetricsGrid(): void {
  const grid = document.getElementById("metrics-grid");
  if (!grid) return;
  const snaps = Object.values(metricsCache).sort((a, b) => a.pluginId.localeCompare(b.pluginId));
  // Streaming redraws (once per pollSecs) don't replay the enter animation: .fresh-tab lingers until the next render(),
  // so after the first frame we add no-enter to the grid to turn it off (same approach as .rpc-no-enter).
  const painted = grid.dataset.metricsPainted === "1";
  grid.classList.toggle("metrics-no-enter", painted);
  grid.dataset.metricsPainted = "1";
  const statuses = snaps.map((s) => metricStatus(s.cpuPct, s.rssBytes, s.threads));
  // hero status dot: take the worst tier across all cards
  const dot = document.getElementById("metrics-dot");
  if (dot) {
    dot.className = `metrics-dot ${statuses.includes("err") ? "err" : statuses.includes("warn") ? "warn" : "ok"}`;
  }
  if (snaps.length === 0) {
    grid.innerHTML = `<div class="setting-hint">${esc(t("metricsEmpty"))}</div>`;
    return;
  }
  const cfg = metricsConfig;
  // Usage bar: current value / threshold (capped at 100%); no bar when the threshold is off or the value is missing
  const barFor = (pct: number | null): string => {
    if (pct === null) return "";
    const band = pct >= 100 ? "err" : pct >= 80 ? "warn" : "ok";
    return `<div class="metrics-bar" aria-hidden="true"><span class="metrics-bar-fill ${band}" style="width:${pct.toFixed(1)}%"></span></div>`;
  };
  grid.innerHTML = snaps
    .map((s) => {
      const status = metricStatus(s.cpuPct, s.rssBytes, s.threads);
      const cpu = s.cpuPct !== null ? `${s.cpuPct.toFixed(1)}%` : "—";
      const rss = fmtBytes(s.rssBytes);
      const threads = s.threads !== null ? String(s.threads) : "—";
      const fds = s.fds !== null ? String(s.fds) : "—";
      const cpuUsage = cfg && cfg.cpuPctMax > 0 && s.cpuPct !== null ? Math.min(100, Math.max(0, (s.cpuPct / cfg.cpuPctMax) * 100)) : null;
      const rssUsage = cfg && cfg.rssBytesMax > 0 && s.rssBytes !== null ? Math.min(100, Math.max(0, (s.rssBytes / cfg.rssBytesMax) * 100)) : null;
      return `<div class="metrics-card metrics-${status}" data-pid="${esc(s.pluginId)}">
        <div class="metrics-card-head"><b>${esc(s.pluginId)}</b><span class="metrics-pid">pid ${s.pid}</span></div>
        <div class="metrics-row"><span class="metrics-label">${esc(t("metricsCpu"))}</span><span class="metrics-value">${cpu}</span></div>
        ${barFor(cpuUsage)}
        <div class="metrics-row"><span class="metrics-label">${esc(t("metricsMemory"))}</span><span class="metrics-value">${rss}</span></div>
        ${barFor(rssUsage)}
        <div class="metrics-row"><span class="metrics-label">${esc(t("metricsThreads"))}</span><span class="metrics-value">${threads}</span></div>
        <div class="metrics-row"><span class="metrics-label">${esc(t("metricsFds"))}</span><span class="metrics-value">${fds}</span></div>
        <canvas class="metrics-spark" data-spark="${esc(s.pluginId)}" width="220" height="40" aria-hidden="true"></canvas>
        <div class="metrics-card-foot"><button class="btn ghost metrics-chart-btn" data-pid="${esc(s.pluginId)}" type="button">${esc(t("metricsChart"))}</button></div>
      </div>`;
    })
    .join("");
  grid.querySelectorAll<HTMLButtonElement>(".metrics-chart-btn").forEach((b) => {
    b.addEventListener("click", () => void openMetricsChart(b.dataset.pid ?? ""));
  });
  // The sparkline canvas is re-rendered with innerHTML; look it up again by data-spark before drawing (not found = tab switched away, give up)
  snaps.forEach((s) => drawMetricsSpark(s.pluginId));
}

/// Get the card's canvas by data-spark: old references go stale after a grid re-render, so look it up again each time.
function sparkCanvasFor(pluginId: string): HTMLCanvasElement | null {
  const grid = document.getElementById("metrics-grid");
  if (!grid) return null;
  for (const c of grid.querySelectorAll<HTMLCanvasElement>("canvas[data-spark]")) {
    if (c.dataset.spark === pluginId) return c;
  }
  return null;
}

/// Card CPU mini chart: single line, no axes, color follows card status; with < 2 points only a dashed baseline is drawn.
function drawMetricsSpark(pluginId: string): void {
  const canvas = sparkCanvasFor(pluginId);
  if (!canvas) return;
  const ctx = canvas.getContext("2d");
  if (!ctx) return;
  const W = canvas.width;
  const H = canvas.height;
  ctx.clearRect(0, 0, W, H);
  const points = (metricsHistory[pluginId] ?? []).slice(-METRICS_SPARK_LIMIT);
  if (points.length < 2) {
    ctx.strokeStyle = "rgba(148, 163, 184, 0.45)";
    ctx.lineWidth = 1;
    ctx.setLineDash([3, 3]);
    ctx.beginPath();
    ctx.moveTo(0, H - 2);
    ctx.lineTo(W, H - 2);
    ctx.stroke();
    ctx.setLineDash([]);
    return;
  }
  const snap = metricsCache[pluginId];
  const status = snap ? metricStatus(snap.cpuPct, snap.rssBytes, snap.threads) : "ok";
  ctx.strokeStyle = status === "err" ? "#f87171" : status === "warn" ? "#fbbf24" : "#4ade80";
  ctx.lineWidth = 1.5;
  const maxCpu = Math.max(60, ...points.map((p) => p.cpu ?? 0));
  ctx.beginPath();
  points.forEach((p, i) => {
    const x = (i / (points.length - 1)) * W;
    const y = H - 2 - ((p.cpu ?? 0) / maxCpu) * (H - 4);
    if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
  });
  ctx.stroke();
}

/// Multi-plugin CPU overview: one polyline per plugin, Y = global peak (floor 60%), no line with < 2 points;
/// Legend = color dot + pluginId + latest value; zero-data series are hidden.
function drawMetricsOverview(): void {
  const canvas = document.getElementById("metrics-overview-canvas") as HTMLCanvasElement | null;
  if (!canvas) return;
  const ctx = canvas.getContext("2d");
  if (!ctx) return;
  const W = canvas.width;
  const H = canvas.height;
  ctx.clearRect(0, 0, W, H);
  metricsOverviewDrawnAt = Date.now();
  const series = Object.keys(metricsHistory)
    .sort((a, b) => a.localeCompare(b))
    .map((id, i) => ({ id, color: METRICS_SERIES_COLORS[i % METRICS_SERIES_COLORS.length], points: metricsHistory[id] ?? [] }))
    .filter((s) => s.points.length > 0);
  const legend = document.getElementById("metrics-overview-legend");
  if (series.length === 0) {
    if (legend) legend.innerHTML = "";
    return;
  }
  let maxCpu = 60;
  for (const s of series) for (const p of s.points) maxCpu = Math.max(maxCpu, p.cpu ?? 0);
  for (const s of series) {
    if (s.points.length < 2) continue;
    ctx.strokeStyle = s.color;
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    s.points.forEach((p, i) => {
      const x = (i / (s.points.length - 1)) * W;
      const y = H - 2 - ((p.cpu ?? 0) / maxCpu) * (H - 4);
      if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
    });
    ctx.stroke();
  }
  if (legend) {
    legend.innerHTML = series
      .map((s) => {
        const latest = s.points.slice().reverse().find((p) => p.cpu !== null)?.cpu ?? null;
        const val = latest !== null ? `${latest.toFixed(1)}%` : "—";
        return `<span class="metrics-ov-chip" title="${esc(s.id)}"><i class="metrics-ov-dot" style="background:${s.color}"></i><span class="metrics-ov-name">${esc(s.id)}</span><span class="metrics-ov-val">${val}</span></span>`;
      })
      .join("");
  }
}

function maybeDrawMetricsOverview(): void {
  if (Date.now() - metricsOverviewDrawnAt < METRICS_OVERVIEW_THROTTLE_MS) return;
  drawMetricsOverview();
}

/// Opening the tab / manual refresh: a single Promise.all fetches all plugin history and overwrites the in-memory ring wholesale; one plugin's failure doesn't affect the rest.
async function refreshMetricsHistories(pluginIds: string[]): Promise<void> {
  if (pluginIds.length === 0) return;
  const alive = new Set(pluginIds);
  for (const key of Object.keys(metricsHistory)) {
    if (!alive.has(key)) delete metricsHistory[key];
  }
  const results = await Promise.all(
    pluginIds.map(async (pluginId) => {
      try {
        const history = await invoke<PluginMetricsSnapshot[]>("get_plugin_metrics_history", {
          args: { pluginId, sinceTs: 0, limit: METRICS_HISTORY_CAP },
        });
        return { pluginId, history };
      } catch {
        return { pluginId, history: null };
      }
    }),
  );
  for (const r of results) {
    if (!r.history || r.history.length === 0) continue;
    metricsHistory[r.pluginId] = r.history.slice(-METRICS_HISTORY_CAP).map((h) => ({ cpu: h.cpuPct, ts: h.ts }));
  }
}

async function openMetricsChart(pluginId: string): Promise<void> {
  if (!pluginId) return;
  const modalId = "metrics-chart-modal";
  document.getElementById(modalId)?.remove();
  const modal = document.createElement("div");
  modal.id = modalId;
  modal.className = "metrics-modal";
  modal.innerHTML = `<div class="metrics-modal-card" role="dialog" aria-modal="true" aria-label="${esc(pluginId)} · ${esc(t("metricsChart"))}">
    <div class="metrics-modal-head">
      <div class="metrics-modal-title"><b>${esc(pluginId)}</b><span class="metrics-modal-legend"><span class="metrics-legend-chip"><i></i>CPU%</span><span class="metrics-legend-chip rss"><i></i>RSS</span></span></div>
      <button class="btn ghost metrics-modal-close" id="metrics-modal-close" type="button" aria-label="${esc(t("dismiss"))}">×</button>
    </div>
    <div class="metrics-modal-body"><canvas id="metrics-chart-canvas" width="600" height="220"></canvas></div>
    <div class="metrics-modal-foot"><span class="setting-hint" id="metrics-chart-msg"></span></div>
  </div>`;
  document.body.appendChild(modal);
  document.getElementById("metrics-modal-close")?.addEventListener("click", () => modal.remove());
  try {
    const history = await invoke<PluginMetricsSnapshot[]>("get_plugin_metrics_history", {
      args: { pluginId, sinceTs: 0, limit: 600 },
    });
    drawMetricsChart(history);
    const msg = document.getElementById("metrics-chart-msg");
    if (msg) msg.textContent = `${history.length} ${esc(t("metricsSamples"))}`;
  } catch (err) {
    const msg = document.getElementById("metrics-chart-msg");
    if (msg) msg.textContent = String(err);
  }
}

function drawMetricsChart(history: PluginMetricsSnapshot[]): void {
  const canvas = document.getElementById("metrics-chart-canvas") as HTMLCanvasElement | null;
  if (!canvas) return;
  const ctx = canvas.getContext("2d");
  if (!ctx) return;
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  if (history.length < 2) return;
  const maxCpu = Math.max(60, ...history.map((h) => h.cpuPct ?? 0));
  const maxRss = Math.max(1, ...history.map((h) => h.rssBytes ?? 0));
  const W = canvas.width, H = canvas.height;
  // CPU line (blue)
  ctx.strokeStyle = "#60a5fa"; ctx.lineWidth = 1.5;
  ctx.beginPath();
  history.forEach((h, i) => {
    const x = (i / (history.length - 1)) * W;
    const y = H - ((h.cpuPct ?? 0) / maxCpu) * H * 0.45;
    if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
  });
  ctx.stroke();
  // RSS line (green)
  ctx.strokeStyle = "#4ade80"; ctx.lineWidth = 1.5;
  ctx.beginPath();
  history.forEach((h, i) => {
    const x = (i / (history.length - 1)) * W;
    const y = H * 0.55 - ((h.rssBytes ?? 0) / maxRss) * H * 0.45;
    if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
  });
  ctx.stroke();
  // Legend
  ctx.font = "11px sans-serif";
  ctx.fillStyle = "#60a5fa"; ctx.fillText("CPU%", 8, 14);
  ctx.fillStyle = "#4ade80"; ctx.fillText("RSS", 8, 28);
}

async function refreshMetricsConfig(): Promise<void> {
  const cfgBox = document.getElementById("metrics-config");
  if (!cfgBox) return;
  try {
    metricsConfig = await invoke<MetricsConfigDto>("get_metrics_config");
    renderMetricsConfig();
  } catch (err) {
    cfgBox.innerHTML = `<div class="setting-hint">${String(err)}</div>`;
  }
}

function renderMetricsConfig(): void {
  const cfgBox = document.getElementById("metrics-config");
  if (!cfgBox || !metricsConfig) return;
  const c = metricsConfig;
  cfgBox.innerHTML = `<div class="metrics-thresh-grid">
    <label class="metrics-field"><span class="metrics-field-label">${esc(t("metricsPollSecs"))}</span><input type="number" id="mc-poll" min="0" max="3600" value="${c.pollSecs}" /></label>
    <label class="metrics-field"><span class="metrics-field-label">${esc(t("metricsCpuMax"))}</span><input type="number" id="mc-cpu" min="0" max="100" value="${c.cpuPctMax}" /></label>
    <label class="metrics-field"><span class="metrics-field-label">${esc(t("metricsRssMax"))}</span><input type="number" id="mc-rss" min="0" step="1" value="${c.rssBytesMax}" /></label>
    <label class="metrics-field"><span class="metrics-field-label">${esc(t("metricsThreadMax"))}</span><input type="number" id="mc-thr" min="0" max="100000" value="${c.threadCountMax}" /></label>
    <label class="metrics-field"><span class="metrics-field-label">${esc(t("metricsKeep"))}</span><input type="number" id="mc-keep" min="100" max="50000" value="${c.keepSamples}" /></label>
  </div>
  <div class="metrics-thresh-actions"><button class="btn" id="mc-save" type="button">${esc(t("metricsSave"))}</button><span class="setting-hint" id="mc-msg"></span></div>`;
  document.getElementById("mc-save")?.addEventListener("click", () => void onSaveMetricsConfig());
}

async function onSaveMetricsConfig(): Promise<void> {
  const msg = document.getElementById("mc-msg");
  const next: MetricsConfigDto = {
    pollSecs: Number((document.getElementById("mc-poll") as HTMLInputElement).value),
    cpuPctMax: Number((document.getElementById("mc-cpu") as HTMLInputElement).value),
    rssBytesMax: Number((document.getElementById("mc-rss") as HTMLInputElement).value),
    threadCountMax: Number((document.getElementById("mc-thr") as HTMLInputElement).value),
    keepSamples: Number((document.getElementById("mc-keep") as HTMLInputElement).value),
  };
  try {
    await invoke("set_metrics_config", { cfg: next });
    metricsConfig = next;
    if (msg) msg.textContent = t("metricsSaved");
    void refreshMetrics();
  } catch (err) {
    if (msg) msg.textContent = `${t("metricsSaveFailed")}: ${String(err)}`;
  }
}

function startMetricsStream(): void {
  stopMetricsStream();
  metricsUnlistenSampled = onEvent("plugin.metrics.sampled", (ev) => {
    const p = ev.payload as { snapshots?: PluginMetricsSnapshot[] } | null;
    if (!p || !Array.isArray(p.snapshots)) return;
    for (const s of p.snapshots) {
      metricsCache[s.pluginId] = s;
      pushHistory(s.pluginId, s.cpuPct, s.ts);
      if (metricsConfig && metricStatus(s.cpuPct, s.rssBytes, s.threads) === "err") {
        // Real-time card coloring doesn't need a toast (toasts go through metrics.exceeded)
      }
    }
    renderMetricsGrid();
    // Sampling only appends to the in-memory ring then lightly repaints: the sparkline is looked up again by data-spark, the overview is throttled to 2s; never refetch history
    p.snapshots.forEach((s) => drawMetricsSpark(s.pluginId));
    maybeDrawMetricsOverview();
  });
  metricsUnlistenExceeded = onEvent("plugin.metrics.exceeded", (ev) => {
    const p = ev.payload as { pluginId?: string; kind?: string; value?: number; threshold?: number } | null;
    if (!p || !p.pluginId) return;
    const msg = document.getElementById("metrics-msg");
    if (msg) msg.textContent = `⚠ ${p.pluginId} ${p.kind}: ${p.value} > ${p.threshold}`;
  });
}

export function stopMetricsStream(): void {
  metricsUnlistenSampled?.();
  metricsUnlistenSampled = null;
  metricsUnlistenExceeded?.();
  metricsUnlistenExceeded = null;
}
