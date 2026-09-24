import { invoke } from "@tauri-apps/api/core";
import { t } from "../i18n";
import { showWarningConfirmDialog } from "./dialogs";
import { esc, formatLifecycleTime, group } from "./shared";

export function renderMarket(body: HTMLElement): void {
  body.innerHTML = group("tabMarket",
    `<div class="setting-row vertical"><span class="setting-hint">${esc(t("marketHint"))}</span><div><button class="btn ghost" id="market-refresh" type="button">${esc(t("marketRefresh"))}</button><span class="setting-hint" id="market-msg"></span></div><div id="market-list"></div></div>`);
  document.getElementById("market-refresh")?.addEventListener("click", () => void refreshMarket(true));
  void refreshMarket(false);
}

interface PluginRatingDto {
  id: string;
  pluginId: string;
  score: number;
  comment?: string;
  ts: number;
}

interface PluginRatingSummaryDto {
  pluginId: string;
  count: number;
  avg: number;
}

function starRowHtml(pluginId: string, current: number): string {
  // 5 stars: each is a button; hover/click selects the score. current is 0..5, default 0.
  let html = `<div class="star-row" data-plugin-id="${esc(pluginId)}">`;
  for (let i = 1; i <= 5; i++) {
    const filled = i <= current ? " filled" : "";
    html += `<button class="star-btn${filled}" type="button" data-star="${i}" aria-label="${i}">★</button>`;
  }
  html += `</div>`;
  return html;
}

function ratingListHtml(rows: PluginRatingDto[]): string {
  if (rows.length === 0) return `<p class="muted">${esc(t("marketNoRatings"))}</p>`;
  return `<ul class="rating-list">${rows
    .map(
      (r) =>
        `<li><span class="rating-score">${"★".repeat(r.score)}${"☆".repeat(5 - r.score)}</span>${
          r.comment ? `<span class="rating-comment">${esc(r.comment)}</span>` : ""
        }<span class="rating-ts">${esc(formatLifecycleTime(r.ts))}</span></li>`,
    )
    .join("")}</ul>`;
}

async function refreshMarket(refresh: boolean): Promise<void> {
  const box = document.getElementById("market-list");
  const msg = document.getElementById("market-msg");
  if (!box) return;
  try {
    const entries = await invoke<MarketEntry[]>("list_marketplace", { refresh });
    if (entries.length === 0) {
      box.innerHTML = `<p class="empty">${esc(t("marketNone"))}</p>`;
      if (msg) msg.textContent = "";
      return;
    }
    // Concurrently fetch each item's summary + first 3 comments; on failure show 'No rating'
    const enriched = await Promise.all(
      entries.map(async (e) => {
        try {
          const [summary, list] = await Promise.all([
            invoke<PluginRatingSummaryDto>("plugin_rating_summary", { id: e.id }),
            invoke<PluginRatingDto[]>("list_plugin_ratings", { id: e.id, limit: 3 }),
          ]);
          return { e, summary, list };
        } catch {
          return { e, summary: { pluginId: e.id, count: 0, avg: 0 } as PluginRatingSummaryDto, list: [] as PluginRatingDto[] };
        }
      }),
    );
    box.innerHTML = enriched
      .map(
        ({ e, summary, list }) =>
          `<div class="market-card"><div class="sess"><b>${esc(e.name)}</b><span class="msg">${esc(e.id)} · v${esc(e.version)}</span><button data-market-install="${esc(e.id)}" type="button">${esc(t("marketInstall"))}</button></div>` +
          (e.description ? `<div class="setting-hint">${esc(e.description)}</div>` : "") +
          (e.capabilities.length || e.permissions.length
            ? `<div class="setting-hint">${esc(e.capabilities.join(" · "))}${e.permissions.length ? " (" + e.permissions.join(", ") + ")" : ""}</div>`
            : "") +
          `<div class="market-rating"><span class="market-avg">${summary.count > 0 ? `★ ${summary.avg.toFixed(2)} · ${summary.count} ${esc(t("marketRatings"))}` : esc(t("marketNoRatings"))}</span></div>` +
          `<div class="market-rate-form"><span class="market-rate-label">${esc(t("marketRate"))}</span>${starRowHtml(e.id, 0)}<input class="market-rate-comment" type="text" placeholder="${esc(t("marketRatePlaceholder"))}" maxlength="280"/><button class="btn ghost" data-market-rate-submit="${esc(e.id)}" type="button">${esc(t("marketRateSubmit"))}</button></div>` +
          `<div class="market-ratings-list">${ratingListHtml(list)}</div>` +
          `</div>`,
      )
      .join("");
    if (msg) msg.textContent = `${entries.length} ${esc(t("marketCount"))}`;
    bindMarketHandlers(box);
  } catch (e) {
    if (msg) msg.textContent = `✗ ${String(e)}`;
  }
}

function bindMarketHandlers(box: HTMLElement): void {
  box.querySelectorAll("button[data-market-install]").forEach((b) => {
    b.addEventListener("click", async () => {
      const id = (b as HTMLElement).dataset.marketInstall ?? "";
      const msg = document.getElementById("market-msg");
      if (msg) msg.textContent = `… ${id}`;
      try {
        let installed: string;
        try {
          installed = await invoke<string>("install_marketplace", { id });
        } catch (e) {
          // F7 — soft-warning tiers require explicit confirmation (unsigned / key change); error flags are emitted by core.
          const text = String(e);
          const needsUnsigned = text.includes("unsigned-confirm-required");
          const needsKeyChange = text.includes("publisher-key-change-confirm-required");
          if (!needsUnsigned && !needsKeyChange) throw e;
          const go = await showWarningConfirmDialog({
            title: t("pluginRiskConfirmTitle"),
            body: text,
            confirmLabel: t("installPreviewConfirm"),
          });
          if (!go) {
            if (msg) msg.textContent = "";
            return;
          }
          installed = await invoke<string>("install_marketplace", {
            id,
            confirmUnsigned: needsUnsigned,
            confirmKeyChange: needsKeyChange,
          });
        }
        if (msg) msg.textContent = `✓ ${installed}`;
        await refreshMarket(false);
      } catch (e) {
        if (msg) msg.textContent = `✗ ${String(e)}`;
      }
    });
  });
  // Star bar hover preview + click writes the score
  box.querySelectorAll(".star-row").forEach((row) => {
    const pid = (row as HTMLElement).dataset.pluginId ?? "";
    row.querySelectorAll("button.star-btn").forEach((btn) => {
      btn.addEventListener("mouseenter", () => {
        const v = Number((btn as HTMLElement).dataset.star ?? "0");
        row.querySelectorAll<HTMLButtonElement>("button.star-btn").forEach((s, idx) => {
          if (idx < v) s.classList.add("hover"); else s.classList.remove("hover");
        });
      });
      btn.addEventListener("mouseleave", () => {
        row.querySelectorAll<HTMLButtonElement>("button.star-btn").forEach((s) => s.classList.remove("hover"));
      });
      btn.addEventListener("click", () => {
        const v = Number((btn as HTMLElement).dataset.star ?? "0");
        row.querySelectorAll<HTMLButtonElement>("button.star-btn").forEach((s, idx) => {
          if (idx < v) s.classList.add("filled"); else s.classList.remove("filled");
        });
        (row as HTMLElement).dataset.picked = String(v);
      });
    });
  });
  // Submit rating
  box.querySelectorAll("button[data-market-rate-submit]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const htmlBtn = btn as HTMLButtonElement;
      const id = htmlBtn.dataset.marketRateSubmit ?? "";
      const card = btn.closest(".market-card");
      const row = card?.querySelector(".star-row");
      const commentInput = card?.querySelector(".market-rate-comment") as HTMLInputElement | null;
      const picked = Number((row as HTMLElement)?.dataset.picked ?? "0");
      if (!picked) {
        const msg = document.getElementById("market-msg");
        if (msg) msg.textContent = `✗ ${t("marketRatePickStars")}`;
        return;
      }
      const comment = commentInput?.value.trim() || null;
      htmlBtn.disabled = true;
      try {
        await invoke("rate_plugin", { id, score: picked, comment });
        await refreshMarket(false);
      } catch (e) {
        const msg = document.getElementById("market-msg");
        if (msg) msg.textContent = `✗ ${String(e)}`;
      } finally {
        htmlBtn.disabled = false;
      }
    });
  });
}

interface MarketEntry {
  id: string;
  name: string;
  version: string;
  description: string;
  download_url: string;
  sha256: string;
  capabilities: string[];
  permissions: string[];
}
