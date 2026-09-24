import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { onEvent } from "../events";
import { t } from "../i18n";
import { esc, escAttr } from "./shared";
import type { HotkeyAction, PaletteEntry } from "./types";

/// Install-time permission ask modal. Core emits `app.emit("opencapx-install-ask")` per permission during the commit phase,
/// and the native prompt only draws in the **pet bubble** — the user is in the settings window clicking install right now, so a prompt on the pet side is as good as
/// invisible, and it then silently waits the full 60s before failing. This adds a modal: whoever answers first wins
/// (`resolve_ask` is idempotent), and after one side answers the other is collapsed by `-done`.
let installAskEl: HTMLElement | null = null;

function showInstallAskModal(p: {
  id: string;
  pluginId: string;
  permission: string;
  canAlways?: boolean;
}): void {
  closeInstallAskModal();
  const overlay = document.createElement("div");
  overlay.id = "install-ask-overlay";
  overlay.style.cssText =
    "position:fixed;inset:0;background:rgba(0,0,0,0.55);z-index:10000;display:flex;align-items:center;justify-content:center;backdrop-filter:blur(4px);";
  overlay.innerHTML = `<div class="install-dialog" role="dialog" aria-label="${esc(t("installConfirm"))}">
    <h2>${esc(t("installConfirm"))}</h2>
    <p class="install-desc">${esc(p.pluginId)} → <code>${esc(p.permission)}</code></p>
    <div class="install-actions"><div class="install-buttons">
      <button class="btn ghost" data-ask="deny" type="button">${esc(t("permDeny"))}</button>
      ${p.canAlways ? `<button class="btn ghost" data-ask="always" type="button">${esc(t("permAlways"))}</button>` : ""}
      <button class="btn primary" data-ask="once" type="button">${esc(t("permAllowOnce"))}</button>
    </div></div>
  </div>`;
  document.body.appendChild(overlay);
  installAskEl = overlay;
  overlay.querySelectorAll<HTMLElement>("button[data-ask]").forEach((b) => {
    b.addEventListener("click", () => {
      const answer = b.dataset.ask ?? "deny";
      closeInstallAskModal();
      void invoke("answer_install", { id: p.id, answer }).catch(() => undefined);
    });
  });
}

function closeInstallAskModal(): void {
  installAskEl?.remove();
  installAskEl = null;
}

/// Wired once at startup (must not go in render: render runs every time and listeners would accumulate).
export function startInstallAskListener(): void {
  void listen<{ id: string; pluginId: string; permission: string; canAlways?: boolean }>(
    "opencapx-install-ask",
    (ev) => showInstallAskModal(ev.payload),
  );
  // The other side (pet bubble) answered -> collapse this side, so a now-invalid button isn't left behind
  void listen("opencapx-install-ask-done", () => closeInstallAskModal());
}

export function startHotkeyPaletteListener(): void {
  // Global hotkey::open_palette event arrives -> open the command palette modal.
  onEvent("hotkey::open_palette", () => {
    void openCommandPalette();
  });
}

export async function openCommandPalette(): Promise<void> {
  let entries: PaletteEntry[] = [];
  try {
    entries = await invoke<PaletteEntry[]>("list_palette_entries");
  } catch {
    entries = [];
  }
  const overlay = document.createElement("div");
  overlay.style.cssText =
    "position:fixed;inset:0;background:rgba(0,0,0,0.55);z-index:9999;display:flex;align-items:center;justify-content:center;backdrop-filter:blur(4px);";
  overlay.innerHTML = `
    <div class="install-dialog palette-dialog" role="dialog" aria-label="${esc(t("paletteTitle"))}">
      <h2>${esc(t("paletteTitle"))}</h2>
      <input type="text" id="palette-filter" class="palette-filter" placeholder="${esc(t("paletteFilter"))}" autofocus />
      <ul class="palette-list" id="palette-list">${entries
        .map(
          (e) => {
            const derivedAction = e.kind === "builtin" ? e.id.replace(/^builtin:/, "") : (e.capability ?? "");
            return `<li class="palette-row" data-pal-id="${escAttr(e.id)}" data-pal-kind="${escAttr(e.kind)}" data-pal-action="${escAttr(derivedAction)}" data-pal-plugin="${esc(e.plugin_id ?? "")}" data-pal-cap="${esc(e.capability ?? "")}"><span class="palette-row-title">${esc(e.title)}</span><span class="palette-row-kind">${esc(e.kind)}</span></li>`;
          },
        )
        .join("")}</ul>
      <div class="install-actions">
        <button class="btn ghost" id="palette-close" type="button">${esc(t("installPreviewCancel"))}</button>
      </div>
    </div>`;
  document.body.appendChild(overlay);
  const filterEl = overlay.querySelector("#palette-filter") as HTMLInputElement | null;
  filterEl?.focus();
  const close = () => overlay.remove();
  overlay.querySelector("#palette-close")?.addEventListener("click", close);
  overlay.addEventListener("click", (ev) => {
    if (ev.target === overlay) close();
  });
  overlay.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape") {
      close();
    }
  });
  const apply = () => {
    const q = (filterEl?.value ?? "").trim().toLowerCase();
    overlay.querySelectorAll<HTMLElement>("li.palette-row").forEach((li) => {
      const title = (li.querySelector(".palette-row-title")?.textContent ?? "").toLowerCase();
      li.style.display = !q || title.includes(q) ? "" : "none";
    });
  };
  filterEl?.addEventListener("input", apply);
  const pick = async (li: HTMLElement) => {
    const id = li.dataset.palId ?? "";
    const kind = li.dataset.palKind ?? "builtin";
    const action = (li.dataset.palAction ?? "") as HotkeyAction["action"];
    const pluginId = li.dataset.palPlugin ?? "";
    const capability = li.dataset.palCap ?? "";
    close();
    const msg = document.getElementById("hotkey-msg");
    if (kind === "builtin") {
      // builtins already go through hotkey bindings. Selecting a builtin in the palette shows a msg telling the user 'bind a key in the Hotkeys tab'.
      if (msg) msg.textContent = `${t("paletteBuiltinNote")} (${action})`;
      void id;
    } else {
      // plugin capability — the palette is a picker; actually triggering still needs a hotkey binding.
      // A hint here tells the user: record a combo in the Hotkeys tab and it can be invoked directly.
      if (msg) msg.textContent = `${t("palettePluginNote")} ${pluginId} → ${capability}`;
    }
  };
  overlay.querySelectorAll<HTMLElement>("li.palette-row").forEach((li) => {
    li.addEventListener("click", () => void pick(li));
  });
}
