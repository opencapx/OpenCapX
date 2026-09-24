import { invoke } from "@tauri-apps/api/core";
import { t } from "../i18n";
import { esc, group } from "./shared";

interface CapabilityStat {
  capability: string;
  pluginId: string;
  count: number;
  avgMs: number;
  p50Ms: number;
  p95Ms: number;
  lastUsedAt: number;
  failCount: number;
  errors: Record<string, number>;
}

// Draw a set of values as a simple SVG bar chart. Each row is 18 high; bar height = (value / maxValue) * 14.
function barChartSvg(items: { label: string; value: number; tip?: string; color?: string }[], valueSuffix = ""): string {
  if (items.length === 0) return `<p class="muted">${esc(t("statsEmpty"))}</p>`;
  const max = Math.max(...items.map((i) => i.value), 1);
  const rowH = 18;
  const labelW = 160;
  const chartW = 220;
  const totalH = items.length * rowH + 4;
  const rows = items.map((it, idx) => {
    const w = Math.max(1, Math.round((it.value / max) * chartW));
    const y = idx * rowH;
    const color = it.color ?? "#5b9cff";
    return `<g><text x="0" y="${y + 12}" font-size="11" fill="currentColor" font-family="ui-monospace,monospace">${esc(it.label.slice(0, 22))}</text><rect x="${labelW}" y="${y + 3}" width="${w}" height="10" fill="${color}" rx="2"><title>${esc(it.tip ?? `${it.label}: ${it.value}${valueSuffix}`)}</title></rect><text x="${labelW + w + 4}" y="${y + 12}" font-size="10.5" opacity="0.7" font-family="ui-monospace,monospace">${esc(String(it.value) + valueSuffix)}</text></g>`;
  });
  return `<svg viewBox="0 0 ${labelW + chartW + 60} ${totalH}" width="100%" height="${totalH}" style="overflow:visible">${rows.join("")}</svg>`;
}

async function refreshCapabilityStats(): Promise<void> {
  const box = document.getElementById("stats-list");
  const msg = document.getElementById("stats-msg");
  if (!box) return;
  try {
    const rows = await invoke<CapabilityStat[]>("list_capability_stats", { samples: 200 });
    if (rows.length === 0) {
      box.innerHTML = `<p class="empty">${esc(t("statsEmpty"))}</p>`;
      if (msg) msg.textContent = "";
      return;
    }
    // Group by capability, one chart per capability (bar = plugin, height = count)
    const byCap = new Map<string, CapabilityStat[]>();
    for (const r of rows) {
      const list = byCap.get(r.capability) ?? [];
      list.push(r);
      byCap.set(r.capability, list);
    }
    const sections: string[] = [];
    for (const [cap, list] of byCap) {
      const callBars = barChartSvg(
        list.map((r) => ({
          label: r.pluginId,
          value: r.count,
          tip: `${r.pluginId} · ${r.count} calls (${r.count - r.failCount} ok · ${r.failCount} fail) · avg ${r.avgMs}ms · p50 ${r.p50Ms}ms · p95 ${r.p95Ms}ms`,
          color: "#5b9cff",
        })),
      );
      const latBars = barChartSvg(
        list.map((r) => ({
          label: r.pluginId,
          value: r.p95Ms,
          tip: `${r.pluginId} p95 ${r.p95Ms}ms / avg ${r.avgMs}ms / p50 ${r.p50Ms}ms`,
          color: "#ffc850",
        })),
        " ms",
      );
      // Phase 31 — failure bucket chart: one error type per row, colored by severity (timeout red / denied orange / err gray)
      const failRows: { label: string; value: number; tip: string; color: string }[] = [];
      for (const r of list) {
        if (r.failCount === 0) continue;
        const total = r.failCount;
        const okCount = r.count - r.failCount;
        const pct = r.count > 0 ? Math.round((okCount / r.count) * 100) : 0;
        // Aggregate errors by kind (merging multiple plugins of the same kind into one row is clearer)
        const kinds = Object.entries(r.errors).sort((a, b) => b[1] - a[1]);
        for (const [kind, count] of kinds) {
          let color = "#9ca3af"; // err default gray
          if (kind === "timeout") color = "#ef4444"; // red
          else if (kind === "permission_denied") color = "#f97316"; // orange
          failRows.push({
            label: `${r.pluginId} · ${kind}`,
            value: count,
            tip: `${r.pluginId} ${kind}: ${count}/${total} fails (${pct}% ok overall)`,
            color,
          });
        }
      }
      const failBars = failRows.length === 0
        ? `<p class="muted">✓ ${esc(t("statsNoFails"))}</p>`
        : barChartSvg(failRows);
      sections.push(
        `<div class="stat-card"><div class="stat-card-title">${esc(cap)}</div><div class="stat-sub">${esc(t("statsCalls"))}</div>${callBars}<div class="stat-sub">${esc(t("statsP95"))}</div>${latBars}<div class="stat-sub">${esc(t("statsFailures"))}</div>${failBars}</div>`,
      );
    }
    box.innerHTML = sections.join("");
    if (msg) msg.textContent = `${rows.length} ${esc(t("statsRows"))}`;
  } catch (e) {
    if (msg) msg.textContent = `✗ ${String(e)}`;
  }
}

export function renderStats(body: HTMLElement): void {
  body.innerHTML = group("tabStats",
    `<div class="setting-row vertical"><span class="setting-hint">${esc(t("statsHint"))}</span><div><button class="btn ghost" id="stats-refresh" type="button">${esc(t("statsRefresh"))}</button><span class="setting-hint" id="stats-msg"></span></div><div id="stats-list"></div></div>`);
  document.getElementById("stats-refresh")?.addEventListener("click", () => void refreshCapabilityStats());
  void refreshCapabilityStats();
}
