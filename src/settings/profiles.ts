import { invoke } from "@tauri-apps/api/core";
import { onEvent } from "../events";
import { t } from "../i18n";
import { esc, escAttr, group } from "./shared";

// ─── Phase 46: Workspace profiles ─────────────────────────────────────────────

interface ProfileInfo {
  name: string;
  isActive: boolean;
  pluginCount: number;
  createdAt: number;
}
let profileUnlisten: (() => void) | null = null;

async function refreshProfiles(): Promise<void> {
  const grid = document.getElementById("profiles-grid");
  const msg = document.getElementById("profiles-msg");
  if (!grid) return;
  try {
    const profiles = await invoke<ProfileInfo[]>("list_workspace_profiles");
    renderProfiles(profiles);
    const active = profiles.find((p) => p.isActive);
    if (msg) msg.textContent = active ? `✓ ${active.name} · ${profiles.length} ${esc(t("profilesCount"))}` : "";
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

function renderProfiles(profiles: ProfileInfo[]): void {
  const grid = document.getElementById("profiles-grid");
  if (!grid) return;
  if (profiles.length === 0) {
    grid.innerHTML = `<div class="setting-hint">${esc(t("profilesEmpty"))}</div>`;
    return;
  }
  grid.innerHTML = profiles
    .map((p) => {
      const activeClass = p.isActive ? " profiles-active" : "";
      const badge = p.isActive ? `<span class="profiles-badge">${esc(t("profilesActive"))}</span>` : "";
      const dateStr = new Date(p.createdAt * 1000).toLocaleString();
      return `<div class="profiles-card${activeClass}" data-pname="${escAttr(p.name)}">
        <div class="metrics-card-head"><b>${esc(p.name)}</b>${badge}</div>
        <div class="profiles-meta">${p.pluginCount} ${esc(t("profilesPlugins"))} · ${esc(dateStr)}</div>
        <div class="metrics-card-foot">
          ${p.isActive ? "" : `<button class="btn profiles-switch-btn" data-pname="${escAttr(p.name)}" type="button">${esc(t("profilesSwitch"))}</button>`}
          ${p.name === "default" ? "" : `<button class="btn ghost profiles-delete-btn" data-pname="${escAttr(p.name)}" type="button">${esc(t("profilesDelete"))}</button>`}
        </div>
      </div>`;
    })
    .join("");
  grid.querySelectorAll<HTMLButtonElement>(".profiles-switch-btn").forEach((b) => {
    b.addEventListener("click", () => void onSwitchProfile(b.dataset.pname ?? ""));
  });
  grid.querySelectorAll<HTMLButtonElement>(".profiles-delete-btn").forEach((b) => {
    b.addEventListener("click", () => void onDeleteProfile(b.dataset.pname ?? ""));
  });
}

async function onCreateProfile(): Promise<void> {
  const input = document.getElementById("profiles-name") as HTMLInputElement | null;
  const msg = document.getElementById("profiles-msg");
  const name = (input?.value ?? "").trim();
  if (!name) {
    if (msg) msg.textContent = t("profilesCreatePlaceholder");
    return;
  }
  if (!window.confirm(t("profilesCreateConfirm").replace("{name}", name))) return;
  try {
    const info = await invoke<ProfileInfo>("create_workspace_profile", { name });
    if (input) input.value = "";
    if (msg) msg.textContent = `✓ ${info.name}`;
    void refreshProfiles();
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

async function onSwitchProfile(name: string): Promise<void> {
  if (!name) return;
  const msg = document.getElementById("profiles-msg");
  if (!window.confirm(t("profilesSwitchConfirm").replace("{name}", name))) return;
  try {
    await invoke("switch_workspace_profile", { name });
    if (msg) msg.textContent = `✓ → ${name}`;
    void refreshProfiles();
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

async function onDeleteProfile(name: string): Promise<void> {
  if (!name) return;
  const msg = document.getElementById("profiles-msg");
  if (!window.confirm(t("profilesDeleteConfirm").replace("{name}", name))) return;
  try {
    await invoke("delete_workspace_profile", { name });
    if (msg) msg.textContent = `✓ deleted ${name}`;
    void refreshProfiles();
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

function startWorkspaceListener(): void {
  stopWorkspaceListener();
  profileUnlisten = onEvent("workspace.switched", () => void refreshProfiles());
}

export function stopWorkspaceListener(): void {
  profileUnlisten?.();
  profileUnlisten = null;
}

export function renderProfilesTab(body: HTMLElement): void {
  body.innerHTML = group("tabProfiles",
    `<div class="setting-row vertical"><span class="setting-hint">${esc(t("profilesHint"))}</span><div><button class="btn ghost" id="profiles-refresh" type="button">${esc(t("profilesRefresh"))}</button><span class="setting-hint" id="profiles-msg"></span></div><div id="profiles-grid"></div><div class="profiles-create-row"><input class="logs-input" id="profiles-name" placeholder="${esc(t("profilesCreatePlaceholder"))}" type="text"/><button class="btn" id="profiles-create" type="button">${esc(t("profilesCreate"))}</button></div></div>`);
  document.getElementById("profiles-refresh")?.addEventListener("click", () => void refreshProfiles());
  document.getElementById("profiles-create")?.addEventListener("click", () => void onCreateProfile());
  void refreshProfiles();
  startWorkspaceListener();
}
