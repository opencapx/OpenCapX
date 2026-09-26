import { invoke } from "@tauri-apps/api/core";
import { t } from "../../i18n";
import { esc, escAttr } from "../shared";
import { formatUnix, weekdayBitsToLabels } from "./shared";

// ─── Phase 50: silences + acks ────────────────────────────────────────────────
interface SilenceRuleDto {
  id: string;
  name: string;
  kindPattern: string;
  startsAt: number;        // unix seconds
  endsAt: number;
  weekdays: number;        // bitmask Mon=1<<0 ... Sun=1<<6
  startHour: number;       // 0..24 UTC hour
  endHour: number;
  createdAt: number;
}

interface AckRuleDto {
  id: string;
  kindPattern: string;
  ackUntil: number;        // unix seconds
  createdAt: number;
}

function silenceIsActive(s: SilenceRuleDto, nowSec: number): boolean {
  if (nowSec < s.startsAt || nowSec >= s.endsAt) return false;
  const wd = new Date(nowSec * 1000).getUTCDay(); // 0=Sun..6=Sat
  const bit = (wd + 6) % 7; // Mon=0..Sun=6
  if (!(s.weekdays & (1 << bit))) return false;
  const h = new Date(nowSec * 1000).getUTCHours();
  if (h < s.startHour || h >= s.endHour) return false;
  return true;
}

export async function refreshAlertingSilences(): Promise<void> {
  const list = document.getElementById("alerting-silences-list");
  const msg = document.getElementById("alerting-silence-msg");
  if (!list) return;
  try {
    const items = await invoke<SilenceRuleDto[]>("list_alerting_silences");
    if (items.length === 0) {
      list.innerHTML = `<span class="setting-hint">${esc(t("alertingSilencesEmpty"))}</span>`;
      return;
    }
    const nowSec = Math.floor(Date.now() / 1000);
    list.innerHTML = items
      .map((s) => {
        const active = silenceIsActive(s, nowSec);
        const status = active
          ? `<span class="alerting-badge alerting-badge-on">${esc(t("alertingSilenceActive"))}</span>`
          : `<span class="alerting-badge">${esc(t("alertingSilenceExpired"))}</span>`;
        return `<div class="silence-card${active ? " silence-card-active" : ""}">
          <div class="silence-card-head">
            <b>${esc(s.name)}</b>
            ${status}
          </div>
          <div class="silence-card-row"><span class="setting-hint">${esc(t("alertingSilencePattern"))}:</span> <code>${esc(s.kindPattern)}</code></div>
          <div class="silence-card-row"><span class="setting-hint">${esc(t("alertingSilenceStart"))}:</span> ${esc(formatUnix(s.startsAt))}</div>
          <div class="silence-card-row"><span class="setting-hint">${esc(t("alertingSilenceEnd"))}:</span> ${esc(formatUnix(s.endsAt))}</div>
          <div class="silence-card-row"><span class="setting-hint">${esc(t("alertingSilenceWeekdays"))}:</span> ${esc(weekdayBitsToLabels(s.weekdays))}</div>
          <div class="silence-card-row"><span class="setting-hint">${esc(t("alertingSilenceStartHour"))}–${esc(t("alertingSilenceEndHour"))}:</span> ${s.startHour}–${s.endHour}</div>
          <div class="silence-card-foot"><button class="btn ghost" data-silence-del="${escAttr(s.id)}" type="button">${esc(t("alertingSilenceDelete"))}</button></div>
        </div>`;
      })
      .join("");
    list.querySelectorAll<HTMLButtonElement>("[data-silence-del]").forEach((btn) => {
      btn.addEventListener("click", () => void deleteSilence(btn.dataset.silenceDel || ""));
    });
    if (msg) msg.textContent = "";
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

async function deleteSilence(id: string): Promise<void> {
  if (!id) return;
  if (!window.confirm(t("alertingSilenceDeleteConfirm"))) return;
  const msg = document.getElementById("alerting-silence-msg");
  try {
    await invoke<boolean>("delete_alerting_silence", { id });
    if (msg) msg.textContent = t("alertingSilenceDeleted");
    void refreshAlertingSilences();
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

export async function onAddSilence(): Promise<void> {
  const msg = document.getElementById("alerting-silence-msg");
  const name = window.prompt(t("alertingSilenceNamePrompt"), "maintenance");
  if (!name) return;
  const pattern = window.prompt(t("alertingSilencePatternPrompt"), "*") || "*";
  const hoursStr = window.prompt(t("alertingSilenceHoursPrompt"), "0-24");
  const days = (window.prompt(t("alertingSilenceDaysPrompt"), "7") || "7").trim();
  if (!hoursStr || !days) return;
  const m = hoursStr.match(/^(\d{1,2})-(\d{1,2})$/);
  if (!m) {
    if (msg) msg.textContent = t("alertingSilenceHoursInvalid");
    return;
  }
  const sh = parseInt(m[1], 10);
  const eh = parseInt(m[2], 10);
  const dayCount = parseInt(days, 10);
  if (sh < 0 || eh > 24 || sh >= eh || isNaN(dayCount) || dayCount < 1 || dayCount > 7) {
    if (msg) msg.textContent = t("alertingSilenceHoursInvalid");
    return;
  }
  // Build the weekday bitmask: the next dayCount days (including today)
  const nowSec = Math.floor(Date.now() / 1000);
  const wd = new Date(nowSec * 1000).getUTCDay(); // 0=Sun..6=Sat
  let weekdays = 0;
  for (let i = 0; i < dayCount; i++) {
    const bit = (wd + 6 + i) % 7;
    weekdays |= 1 << bit;
  }
  try {
    await invoke("save_alerting_silence", {
      silence: {
        id: "",
        name,
        kindPattern: pattern,
        startsAt: nowSec,
        endsAt: nowSec + dayCount * 86400,
        weekdays,
        startHour: sh,
        endHour: eh,
      },
    });
    if (msg) msg.textContent = t("alertingSilenceCreated");
    void refreshAlertingSilences();
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

export async function refreshAlertingAcks(): Promise<void> {
  const list = document.getElementById("alerting-acks-list");
  const msg = document.getElementById("alerting-ack-msg");
  if (!list) return;
  try {
    const items = await invoke<AckRuleDto[]>("list_alerting_acks");
    const nowSec = Math.floor(Date.now() / 1000);
    const active = items.filter((a) => a.ackUntil > nowSec);
    if (items.length === 0) {
      list.innerHTML = `<span class="setting-hint">${esc(t("alertingAcksEmpty"))}</span>`;
      return;
    }
    list.innerHTML = items
      .map((a) => {
        const isActive = a.ackUntil > nowSec;
        const status = isActive
          ? `<span class="alerting-badge alerting-badge-on">${esc(t("alertingSilenceActive"))}</span>`
          : `<span class="alerting-badge">${esc(t("alertingSilenceExpired"))}</span>`;
        return `<div class="ack-card${isActive ? " ack-card-active" : ""}">
          <div class="ack-card-head">
            <code>${esc(a.kindPattern)}</code>
            ${status}
          </div>
          <div class="ack-card-row"><span class="setting-hint">${esc(t("alertingAckUntil"))}:</span> ${esc(formatUnix(a.ackUntil))}</div>
          <div class="ack-card-foot"><button class="btn ghost" data-ack-del="${escAttr(a.id)}" type="button">${esc(t("alertingSilenceDelete"))}</button></div>
        </div>`;
      })
      .join("");
    list.querySelectorAll<HTMLButtonElement>("[data-ack-del]").forEach((btn) => {
      btn.addEventListener("click", () => void deleteAck(btn.dataset.ackDel || ""));
    });
    if (msg) msg.textContent = "";
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

async function deleteAck(id: string): Promise<void> {
  if (!id) return;
  const msg = document.getElementById("alerting-ack-msg");
  try {
    await invoke<boolean>("delete_alerting_ack", { id });
    if (msg) msg.textContent = t("alertingSilenceDeleted");
    void refreshAlertingAcks();
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

export async function onAckKind(): Promise<void> {
  const msg = document.getElementById("alerting-ack-msg");
  const patternEl = document.getElementById("alerting-ack-pattern") as HTMLInputElement | null;
  const windowEl = document.getElementById("alerting-ack-window") as HTMLInputElement | null;
  if (!patternEl || !windowEl) return;
  const pattern = patternEl.value.trim();
  if (!pattern) {
    if (msg) msg.textContent = t("alertingAckPatternRequired");
    return;
  }
  const mins = parseInt(windowEl.value, 10);
  if (isNaN(mins) || mins < 1) {
    if (msg) msg.textContent = t("alertingAckWindowInvalid");
    return;
  }
  try {
    await invoke("ack_alerting_kind", { kindPattern: pattern, windowSecs: mins * 60 });
    if (msg) msg.textContent = t("alertingAckCreated");
    void refreshAlertingAcks();
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}
