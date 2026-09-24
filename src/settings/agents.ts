import { invoke } from "@tauri-apps/api/core";
import { t } from "../i18n";
import { esc, escAttr, group } from "./shared";

export function renderAgents(body: HTMLElement): void {
  body.innerHTML = group("tabAgents",
    `<div class="setting-row vertical"><span class="setting-hint">${esc(t("agentsViewHint"))}</span><div class="setting-hint">${esc(t("agentsPermHighRiskHint"))}</span><div><button class="btn ghost" id="agents-id-refresh" type="button">${esc(t("auditRefresh"))}</button><span class="setting-hint" id="agents-id-msg"></span></div><div id="agents-id-list"></div></div>`);
  document.getElementById("agents-id-refresh")?.addEventListener("click", () => void refreshIdAgents());
  void refreshIdAgents();
}

/// docs/permissions.md 'Agent identity': a security principal (the caller of /rpc, /event),
/// a different concept from the General tab's AgentInfo (hook installer) and observed Sessions.
interface AgentIdentityDto {
  agent_id: string;
  kind: string;
  displayName: string;
  status: string;
  firstSeen: number;
  lastSeen: number;
  registeredVia: string;
}

/// agent_permissions view entry (decision = override > default table; default is used by Reset).
interface AgentPermEntry {
  permission: string;
  decision: string;
  default: string;
  high_risk: boolean;
  /// permission-domains §4.3: declared derived permissions are once-only; the UI offers no granted option.
  declared: boolean;
  /// Global policy override ('' = no override); denied is a hard gate that overrides any per-agent granted.
  global: string;
}

/// Agents tab (docs/permissions.md 'Agents view'): identity cards +
/// revoke / re-authorize + per-agent permission decisions (high-risk ones offer no granted, matching the backend's refusal).
/// Every change refreshes the whole card; details' expanded state is preserved across refreshes.
async function refreshIdAgents(): Promise<void> {
  const box = document.getElementById("agents-id-list");
  if (!box) return;
  let list: AgentIdentityDto[] = [];
  try {
    list = await invoke<AgentIdentityDto[]>("agents_list");
  } catch {
    list = [];
  }
  if (list.length === 0) {
    box.innerHTML = `<p class="empty">${esc(t("agentsNone"))}</p>`;
    return;
  }
  // Remember which details are expanded and restore them after a refresh
  const openIds = new Set(
    Array.from(box.querySelectorAll("details[data-agent-details][open]")).map(
      (d) => (d as HTMLDetailsElement).dataset.agentDetails ?? "",
    ),
  );
  const fmt = (secs: number): string => (secs > 0 ? new Date(secs * 1000).toLocaleString() : "—");
  const cards = await Promise.all(
    list.map(async (a) => {
      let perms: AgentPermEntry[] = [];
      try {
        perms = await invoke<AgentPermEntry[]>("agent_permissions", { agentId: a.agent_id });
      } catch {
        perms = [];
      }
      const revoked = a.status === "revoked";
      const statusBadge = revoked
        ? `<span class="plugin-status plugin-status-error">${esc(t("agentsRevoked"))}</span>`
        : `<span class="plugin-status plugin-status-running">${esc(t("agentsActive"))}</span>`;
      const action = revoked
        ? `<button class="btn ghost" data-agent-reauth="${escAttr(a.agent_id)}" type="button">${esc(t("agentsReauthorize"))}</button>`
        : `<button class="btn ghost danger" data-agent-revoke="${escAttr(a.agent_id)}" data-agent-name="${escAttr(a.displayName)}" type="button">${esc(t("agentsRevoke"))}</button>`;
      const permRows = perms
        .map((e) => {
          // docs/permission-domains.md §4.3 enforcement point 3: declared derived permissions offer only ask/denied
          // (Core's set_decision also rejects granted; this just avoids a pointless click)
          const noAlways = e.high_risk || e.declared;
          const opts = noAlways && e.decision !== "granted"
            ? [["ask", t("permAsk")], ["denied", t("permDenied")]]
            : [["granted", t("permGranted")], ["ask", t("permAsk")], ["denied", t("permDenied")]];
          const sel = `<select data-agent-perm="${escAttr(a.agent_id)}" data-perm="${escAttr(e.permission)}">${opts
            .map(([v, l]) => `<option value="${escAttr(v)}"${v === e.decision ? " selected" : ""}>${esc(l)}</option>`)
            .join("")}</select>`;
          const badge = (e.high_risk ? ` <span class="setting-hint">${esc(t("permHighRisk"))}</span>` : "")
            + (e.declared ? ` <span class="setting-hint">${esc(t("permDeclared"))}</span>` : "")
            + (e.global ? ` <span class="setting-hint">${esc(t("corePermGlobalBadge"))}${esc(e.global)}</span>` : "");
          return `<div class="setting-row"><div class="setting-info"><span class="setting-label">${esc(e.permission)}${badge}</span></div><div class="perm-controls">${sel}</div></div>`;
        })
        .join("");
      return `<div class="agent-card">` +
        `<div class="sess plugin-head"><div class="plugin-meta"><b>${esc(a.displayName)}</b> <span class="plugin-id">${esc(a.agent_id)}</span> <span class="plugin-ver">${esc(a.kind)}</span>${statusBadge}</div><div class="plugin-actions">${action}</div></div>` +
        `<div class="plugin-desc"><span class="setting-hint">${esc(t("agentsFirstSeen"))}: ${esc(fmt(a.firstSeen))} · ${esc(t("agentsLastSeen"))}: ${esc(fmt(a.lastSeen))} · ${esc(t("agentsVia"))}: ${esc(a.registeredVia)}</span></div>` +
        `<details data-agent-details="${escAttr(a.agent_id)}"${openIds.has(a.agent_id) ? " open" : ""}><summary class="setting-label">${esc(t("agentsPermsTitle"))}</summary><div class="settings-list">${permRows}</div></details>` +
        `</div>`;
    }),
  );
  box.innerHTML = cards.join("");
  box.querySelectorAll("button[data-agent-revoke]").forEach((b) => {
    b.addEventListener("click", async () => {
      const el = b as HTMLElement;
      if (!window.confirm(t("agentsRevokeConfirm"))) return;
      try {
        await invoke("agent_revoke", { agentId: el.dataset.agentRevoke });
      } catch (err) {
        const m = document.getElementById("agents-id-msg");
        if (m) m.textContent = `${t("agentsRevokeFailed")}: ${(err as Error).message ?? err}`;
        return;
      }
      await refreshIdAgents();
    });
  });
  box.querySelectorAll("button[data-agent-reauth]").forEach((b) => {
    b.addEventListener("click", async () => {
      const el = b as HTMLElement;
      let token: string;
      try {
        token = await invoke<string>("agent_reauthorize", { agentId: el.dataset.agentReauth });
      } catch (err) {
        const m = document.getElementById("agents-id-msg");
        if (m) m.textContent = `✗ ${String(err)}`;
        return;
      }
      showAgentTokenDialog(token);
      await refreshIdAgents();
    });
  });
  box.querySelectorAll("select[data-agent-perm]").forEach((s) => {
    s.addEventListener("change", async () => {
      const el = s as HTMLSelectElement;
      try {
        await invoke("agent_set_permission", { agentId: el.dataset.agentPerm, permission: el.dataset.perm, decision: el.value });
      } catch {
        /* High-risk rejected granted etc., echoed back on refresh */
      }
      await refreshIdAgents();
    });
  });
}

/// One-time re-authorization token dialog: the token appears only once here and never enters any persistent frontend state.
function showAgentTokenDialog(token: string): void {
  const overlay = document.createElement("div");
  overlay.style.cssText =
    "position:fixed;inset:0;background:rgba(0,0,0,0.55);z-index:9999;display:flex;align-items:center;justify-content:center;backdrop-filter:blur(4px);";
  overlay.innerHTML = `
    <div class="install-dialog" role="dialog" aria-label="${esc(t("agentsTokenTitle"))}">
      <h2>${esc(t("agentsTokenTitle"))}</h2>
      <p class="install-desc">${esc(t("agentsTokenOnce"))}</p>
      <div class="install-section">
        <code class="agent-token">${esc(token)}</code>
      </div>
      <div class="install-actions">
        <button class="btn ghost" id="agent-token-copy" type="button">${esc(t("agentsTokenCopy"))}</button>
        <button class="btn primary" id="agent-token-done" type="button">${esc(t("agentsTokenDone"))}</button>
      </div>
    </div>`;
  document.body.appendChild(overlay);
  const copyBtn = overlay.querySelector<HTMLButtonElement>("#agent-token-copy");
  copyBtn?.addEventListener("click", async () => {
    if (!copyBtn) return;
    try {
      await navigator.clipboard.writeText(token);
      copyBtn.textContent = t("agentsTokenCopied");
    } catch {
      window.prompt(t("agentsTokenOnce"), token);
    }
  });
  overlay.querySelector("#agent-token-done")?.addEventListener("click", () => overlay.remove());
  overlay.addEventListener("click", (ev) => {
    if (ev.target === overlay) overlay.remove();
  });
}
