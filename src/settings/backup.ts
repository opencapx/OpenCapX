import { invoke } from "@tauri-apps/api/core";
import { t } from "../i18n";
import { esc, escAttr, formatBytes, group } from "./shared";

// ---- Phase 41 — Backup tab (one-click workspace backup / restore) ---------------------

interface BackupMeta {
  filename: string;
  createdAt: number;
  sizeBytes: number;
  appVersion: string;
  tableCounts: Record<string, number>;
  pluginConfigCount: number;
}

interface RestoreReport {
  filename: string;
  tableCounts: Record<string, number>;
  pluginConfigCount: number;
}

function formatTimestamp(ts: number): string {
  return new Date(ts * 1000).toLocaleString();
}

async function refreshBackups(): Promise<void> {
  const box = document.getElementById("backup-list");
  if (!box) return;
  let items: BackupMeta[] = [];
  try {
    items = await invoke<BackupMeta[]>("list_backups");
  } catch (err) {
    box.innerHTML = `<p class="setting-hint">✗ ${esc(String(err))}</p>`;
    return;
  }
  if (items.length === 0) {
    box.innerHTML = `<p class="empty">${esc(t("backupEmpty"))}</p>`;
    return;
  }
  const summaryKeys = ["plugins", "plugin_channel", "plugin_health_config", "hotkeys"];
  box.innerHTML = items
    .map(
      (b) => {
        const summary = summaryKeys
          .filter((k) => (b.tableCounts[k] ?? 0) > 0)
          .map((k) => `${k}=${b.tableCounts[k]}`)
          .join(" · ");
        return `<div class="sess backup-row" data-backup-file="${escAttr(b.filename)}"><span class="msg"><b>${esc(b.filename)}</b><br><span class="muted">${esc(formatTimestamp(b.createdAt))} · ${esc(formatBytes(b.sizeBytes))} · v${esc(b.appVersion)}</span>${summary ? `<br><span class="muted">${esc(summary)}</span>` : ""}${b.pluginConfigCount ? ` · configs=${b.pluginConfigCount}` : ""}</span><button class="btn ghost" data-backup-restore="${escAttr(b.filename)}" type="button">${esc(t("backupRestore"))}</button><button class="btn ghost danger" data-backup-delete="${escAttr(b.filename)}" type="button">${esc(t("remove"))}</button></div>`;
      },
    )
    .join("");
  box.querySelectorAll<HTMLButtonElement>("button[data-backup-restore]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const file = btn.dataset.backupRestore ?? "";
      if (!window.confirm(`${t("backupRestoreConfirm")}\n\n${file}`)) return;
      try {
        const report = await invoke<RestoreReport>("restore_backup", { filename: file });
        const msg = document.getElementById("backup-msg");
        if (msg) msg.textContent = `${t("backupRestored")}: ${Object.entries(report.tableCounts).filter(([, n]) => n > 0).map(([k, n]) => `${k}=${n}`).join(", ") || t("backupEmpty")}`;
        await refreshBackups();
      } catch (err) {
        const m = document.getElementById("backup-msg");
        if (m) m.textContent = `${t("backupRestoreFailed")}: ${(err as Error).message ?? err}`;
      }
    });
  });
  box.querySelectorAll<HTMLButtonElement>("button[data-backup-delete]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const file = btn.dataset.backupDelete ?? "";
      if (!window.confirm(`${t("backupDeleteConfirm")}\n\n${file}`)) return;
      try {
        await invoke("delete_backup", { filename: file });
        await refreshBackups();
      } catch (err) {
        const m = document.getElementById("backup-msg");
        if (m) m.textContent = `${t("backupDeleteFailed")}: ${(err as Error).message ?? err}`;
      }
    });
  });
}

async function onCreateBackup(): Promise<void> {
  const btn = document.getElementById("backup-create") as HTMLButtonElement | null;
  if (btn) btn.disabled = true;
  try {
    const meta = await invoke<BackupMeta>("create_backup");
    const msg = document.getElementById("backup-msg");
    if (msg) msg.textContent = `${t("backupCreated")}: ${meta.filename} (${formatBytes(meta.sizeBytes)})`;
    await refreshBackups();
  } catch (err) {
    const m = document.getElementById("backup-msg");
    if (m) m.textContent = `${t("backupCreateFailed")}: ${(err as Error).message ?? err}`;
  } finally {
    if (btn) btn.disabled = false;
  }
}

export function renderBackup(body: HTMLElement): void {
  body.innerHTML = group("tabBackup",
    `<div class="setting-row vertical"><span class="setting-hint">${esc(t("backupHint"))}</span><div><button class="btn" id="backup-create" type="button">${esc(t("backupCreate"))}</button><button class="btn ghost" id="backup-refresh" type="button">${esc(t("auditRefresh"))}</button><span class="setting-hint" id="backup-msg"></span></div><div id="backup-list"></div></div>`);
  document.getElementById("backup-create")?.addEventListener("click", () => void onCreateBackup());
  document.getElementById("backup-refresh")?.addEventListener("click", () => void refreshBackups());
  void refreshBackups();
}
