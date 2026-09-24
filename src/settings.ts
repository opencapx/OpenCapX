import "./settings.css";
import { invoke } from "@tauri-apps/api/core";
import { isEnabled } from "@tauri-apps/plugin-autostart";
import { open } from "@tauri-apps/plugin-dialog";
import { availableLocales, getLocale, setLocale, t, type I18nKey, type Locale } from "./i18n";
import { ICON_ABOUT, ICON_AGENTS, ICON_AUDIT, ICON_AUTOMATION, ICON_BUBBLE, ICON_CONFIG, ICON_GENERAL, ICON_LOGS, ICON_NOTIFY, ICON_PET, ICON_PLUGINS, ICON_SLA, ICON_STATS } from "./icons";
import { getVersion } from "@tauri-apps/api/app";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  deletePack,
  forgetModelPack,
  forgetPack,
  importPack,
  listPacks,
  loadImage,
  loadModelPack,
  loadPack,
  sliceSheet,
  type PackFrame,
} from "./petpack";
import { renderModelSnapshot } from "./pet3d";
import { MOOD_ROWS } from "./sprite";
import {
  isSoundEnabled,
  setCustomSound,
  type SoundKind,
} from "./sounds";
import {
  disconnectEventStream,
} from "./events";
import {
  applyTheme,
  bindToggles,
  cssEscape,
  esc,
  escAttr,
  getAppVersion,
  getPluginNameById,
  getPluginPageReturn,
  getSettings,
  getTab,
  group,
  load,
  loadDbRecoveryNotice,
  paintSessions,
  refreshPluginConfig,
  refreshSessions,
  registerLegacyCleanup,
  registerLegacyRenderer,
  registerTab,
  render,
  row,
  save,
  segmented,
  setAppVersion,
  setSettings,
  setWindowFocused,
  startThemeListener,
  switchTab,
  toggle,
  type Tab,
} from "./settings/shared";
import type {
  Cond,
  LocalizedText,
  PluginConfigRow,
  PluginSettingsView,
  SettingDecl,
  ValidateRule,
} from "./settings/types";
import { renderAgents } from "./settings/agents";
import { renderAudit, stopAuditStream } from "./settings/audit";
import { renderAutomation } from "./settings/automation";
import { renderBubbleSettings } from "./settings/bubble";
import { renderBackup } from "./settings/backup";
import { renderCapabilities } from "./settings/capabilities";
import { renderHotkeys } from "./settings/hotkeys";
import { renderLogs, stopLogsStream } from "./settings/logs";
import { renderMarket } from "./settings/market";
import { renderMetricsTab, stopMetricsStream } from "./settings/metrics";
import { renderNotify } from "./settings/notify";
import { renderProfilesTab, stopWorkspaceListener } from "./settings/profiles";
import { refreshPlugins, renderPlugins } from "./settings/plugins";
import { renderRules } from "./settings/rules";
import { renderStats } from "./settings/stats";
import { renderSla } from "./settings/sla";
import { renderRpcTrace } from "./settings/rpc-trace";
import { startInstallAskListener } from "./settings/modals";

// Settings · Pet tab preview state and thumbnail cache.
// Preview state is UI state (not persisted to settings): switching just shows the same pet in different states.
type PetMood = "working" | "waiting" | "done" | "idle";
const PET_PREVIEW_MOODS: readonly PetMood[] = ["working", "waiting", "done", "idle"];
const PET_MOOD_I18N: Record<PetMood, I18nKey> = {
  working: "petStateWorking",
  waiting: "petStateWaiting",
  done: "petStateSuccess",
  idle: "petStateIdle",
};
let petPreviewMood: PetMood = "idle";
/** Cache of petpack thumbnails (first frame), keyed by pack id. `broken` = this pack fails to render. */
const petThumbCache = new Map<string, { url: string; broken: boolean }>();
interface AgentInfo {
  kind: string;
  display_name: string;
  installed: boolean;
  note: string | null;
}

let agents: AgentInfo[] = [];

function renderLegacyTab(body: HTMLElement): void {
  if (getTab() === "general") {
    // The welcome card used to clear this flag; keep clearing it here so the first-run auto-open (overlay.ts) stays one-time
    if (!getSettings().onboarded) {
      setSettings({ ...getSettings(), onboarded: true });
      void save();
    }
    body.innerHTML =
      `<div class="settings-list"><div class="about-card"><div class="logo">${ICON_PET}</div><div><b>OpenCapX</b></div><div class="ver">${esc(t("version"))} ${esc(getAppVersion())}</div><p>${esc(t("aboutText"))}</p></div></div>` +
      group("agents", `<div class="setting-row vertical"><span class="setting-hint">${esc(t("agentsHint"))}</span><div id="agent-list"></div></div>`) +
      group("sessions", `<div class="setting-row vertical"><span class="setting-hint">${esc(t("sessionsHint"))}</span><div id="session-list"></div><div><button class="btn ghost" id="clear" type="button">${esc(t("clearAll"))}</button></div></div>`) +
      group("sounds",
        row("soundDone", "soundDoneHint", toggle("soundDone", isSoundEnabled("done"))) +
        row("soundWaiting", "soundWaitingHint", toggle("soundWaiting", isSoundEnabled("waiting"))) +
        `<div class="setting-row vertical"><div class="scenarios"><label class="btn ghost" for="snd-done-file">${esc(t("soundUpload"))} · ${esc(t("soundDone"))}</label><input type="file" id="snd-done-file" accept="audio/*" hidden /><button class="btn ghost" id="snd-done-reset" type="button">${esc(t("soundReset"))}</button><label class="btn ghost" for="snd-wait-file">${esc(t("soundUpload"))} · ${esc(t("soundWaiting"))}</label><input type="file" id="snd-wait-file" accept="audio/*" hidden /><button class="btn ghost" id="snd-wait-reset" type="button">${esc(t("soundReset"))}</button></div></div>`) +
      group(null,
        row("autostart", "autostartHint", toggle("autostart", false)) +
        // Language list auto-generated: adding src/locales/<code>.json makes it appear here
        row(
          "locale",
          "localeHint",
          `<select id="locale">${availableLocales()
            .map((l) => `<option value="${esc(l.code)}">${esc(l.name)}</option>`)
            .join("")}</select>`,
        ) +
        row("theme", "themeHint", segmented("theme", ["light", "dark", "system"], getSettings().theme)) +
        row("breakReminder", "breakReminderHint", toggle("breakEnabled", getSettings().breakEnabled)) +
        row("breakMinutes", null, `<input type="number" id="bmins" min="5" max="480" value="${getSettings().breakMinutes}" />`) +
        row("sessionContext", "sessionContextHint", toggle("sessionContextInject", getSettings().sessionContextInject !== false)) +
        row("channelDefault", "channelDefaultHint", `<select id="default-channel"><option value="stable">${esc(t("channelStable"))}</option><option value="beta">${esc(t("channelBeta"))}</option><option value="dev">${esc(t("channelDev"))}</option></select><span class="setting-hint" id="channel-msg"></span>`)) +
      group("cliCommand", `<div class="setting-row vertical"><span class="setting-hint">${esc(t("cliCommandHint"))}</span><div class="ks-status" id="cli-status"></div><div><button class="btn ghost" id="cli-toggle" type="button"></button><span class="setting-hint" id="cli-msg"></span></div></div>`) +
      group("killSwitch", `<div class="setting-row vertical"><span class="setting-hint">${esc(t("killSwitchHint"))}</span><div class="ks-status" id="ks-status"></div><div class="ks-controls"><input type="text" id="ks-reason" placeholder="${esc(t("killSwitchReasonPlaceholder"))}" maxlength="120"/><button class="btn danger" id="ks-enable" type="button">${esc(t("killSwitchEnable"))}</button><button class="btn ghost" id="ks-disable" type="button">${esc(t("killSwitchDisable"))}</button></div></div>`) +
      group("safeMode", `<div class="setting-row vertical"><span class="setting-hint">${esc(t("safeModeHint"))}</span><div class="ks-status" id="sm-status"></div></div>`);
    paintSessions();
    void refreshAgents();
    void refreshCliCommand();
    document.getElementById("cli-toggle")?.addEventListener("click", async () => {
      const btn = document.getElementById("cli-toggle") as HTMLButtonElement;
      const msg = document.getElementById("cli-msg");
      btn.disabled = true;
      try {
        const out = await invoke<string>(btn.dataset.mode === "uninstall" ? "cli_command_uninstall" : "cli_command_install");
        if (msg) msg.textContent = out;
      } catch (err) {
        if (msg) msg.textContent = `✗ ${String(err)}`;
      } finally {
        btn.disabled = false;
        void refreshCliCommand();
      }
    });
    document.getElementById("clear")?.addEventListener("click", async () => {
      await invoke("clear_sessions");
      await refreshSessions();
    });
    bindToggles(body);
    wireSoundUpload("snd-done-file", "snd-done-reset", "done");
    wireSoundUpload("snd-wait-file", "snd-wait-reset", "waiting");
    const loc = document.getElementById("locale") as HTMLSelectElement | null;
    if (loc) {
      loc.value = getLocale();
      loc.addEventListener("change", () => {
        const v = loc.value as Locale;
        setLocale(v);
        setSettings({ ...getSettings(), locale: v });
        void save();
        render();
      });
    }
    const auto = body.querySelector('button[data-toggle="autostart"]');
    if (auto) {
      isEnabled().then((v) => auto.classList.toggle("active", v)).catch(() => undefined);
    }
    // Phase 38 — global default update channel
    const dc = document.getElementById("default-channel") as HTMLSelectElement | null;
    if (dc) {
      invoke<string>("get_default_channel")
        .then((cur) => {
          dc.value = cur || "stable";
        })
        .catch(() => {
          dc.value = "stable";
        });
      dc.addEventListener("change", () => {
        const prev = dc.value;
        const msg = document.getElementById("channel-msg");
        invoke("set_default_channel", { channel: dc.value }).then(
          () => { if (msg) msg.textContent = ""; },
          (err) => {
            dc.value = prev;
            if (msg) msg.textContent = `${t("channelSetFailed")}: ${(err as Error).message ?? err}`;
          },
        );
      });
    }
    body.querySelectorAll("button[data-seg='theme']").forEach((b) => {
      b.addEventListener("click", () => {
        setSettings({ ...getSettings(), theme: (b as HTMLElement).dataset.val ?? "system" });
        applyTheme();
        void save();
        render();
      });
    });
    // Phase 44 — Kill switch global disable toggle
    void refreshKillSwitch();
    void refreshSafeMode();
    document.getElementById("ks-enable")?.addEventListener("click", () => void onEnableKillSwitch());
    document.getElementById("ks-disable")?.addEventListener("click", () => void onDisableKillSwitch());
  } else if (getTab() === "pet") {
    // The preview state (switched with the state chips) and the 'current appearance' decision follow the same priority as the overlay:
    // petpack > spritesheet URL > built-in logo.
    const effectiveSource = (): "pack" | "sheet" | "logo" =>
      getSettings().petPack ? "pack" : getSettings().petSheet ? "sheet" : "logo";
    // Data URLs (old local uploads) no longer stuff a long base64 string into the input; show 'Local image' + a thumbnail instead
    const isDataUrl = getSettings().petSheet.startsWith("data:");
    // 'In use / overridden by petpack': the priority is implicit, so it must be stated explicitly
    const flagHtml = (inUse: boolean, overridden: boolean): string => {
      if (inUse) return `<span class="pet-flag on">${esc(t("petInUse"))}</span>`;
      if (overridden) return `<span class="pet-flag">${esc(t("petOverridden"))}</span>`;
      return "";
    };
    const eff = effectiveSource();
    const moodChips = PET_PREVIEW_MOODS.map((m) => {
      const key = PET_MOOD_I18N[m];
      return `<button class="pet-mood${m === petPreviewMood ? " active" : ""}" data-mood="${m}" type="button" aria-pressed="${
        m === petPreviewMood
      }">${esc(t(key))}</button>`;
    }).join("");

    body.innerHTML =
      // 1) Current appearance + state preview: any settings change is visible here immediately
      `<div class="pet-sect">${group(
        null,
        `<div class="pet-hero">
           <div class="pet-stage"><canvas id="pet-preview" width="208" height="208" role="img" aria-label="${esc(t("petPreviewMood"))}"></canvas></div>
           <div class="pet-hero-side">
             <div class="pet-hero-source">
               <span class="pet-hero-label">${esc(t("petInUse"))}</span>
               <span class="pet-hero-value" id="pet-effective">—</span>
             </div>
             <div class="pet-moods" role="group" aria-label="${esc(t("petPreviewMood"))}">${moodChips}</div>
           </div>
         </div>`,
      )}</div>` +
      // 2) Appearance: visibility + size
      `<div class="pet-sect">${group(
        "petAppearance",
        row("petVisible", "petVisibleHint", toggle("petVisible", getSettings().petVisible)) +
          row(
            "petSize",
            "petSizeHint",
            `<input type="range" id="petsize" min="70" max="130" value="${getSettings().petSize}" /><span id="petsize-v" class="pet-num">${getSettings().petSize}%</span>`,
          ),
      )}</div>` +
      // 3) Appearance source: petpack (thumbnail selection) / custom spritesheet URL (advanced)
      `<div class="pet-sect">${group(
        "petImageSource",
        `<div class="setting-row vertical">
           <div class="setting-info"><div class="setting-label-row"><span class="setting-label">${esc(t("petPack"))}</span><span id="pet-flag-pack"></span></div><span class="setting-hint">${esc(t("petPackHint"))}</span></div>
           <div class="pet-grid" id="petpack-grid"></div>
           <p class="setting-hint" id="petpack-empty" hidden>${esc(t("petPackEmpty"))}</p>
           <div class="pet-tools">
             <button class="btn ghost" id="petpack-import" type="button">${esc(t("petPackImport"))}</button>
             <button class="btn ghost" id="petpack-import-image" type="button">${esc(t("petPackImportImage"))}</button>
             <button class="btn ghost" id="petpack-import-model" type="button">${esc(t("petPackImportModel"))}</button>
           </div>
         </div>` +
          `<div class="setting-row vertical pet-adv">
           <div class="setting-info"><div class="setting-label-row"><span class="setting-label">${esc(t("petSourceSheet"))}</span><span id="pet-flag-sheet">${flagHtml(
             eff === "sheet",
             eff === "pack" && !!getSettings().petSheet,
           )}</span></div><span class="setting-hint">${esc(t("petUrlHint"))}</span></div>
           <div class="pet-adv-row">
             ${
               isDataUrl
                 ? `<span class="pet-local">${esc(t("petLocalImage"))}</span>`
                 : `<input type="text" id="petsheet" value="${esc(getSettings().petSheet)}" placeholder="https://…" />
                    <button class="btn ghost" id="petimport-url" type="button">${esc(t("petImportFromUrl"))}</button>`
             }
             <button class="btn ghost" id="petreset" type="button">${esc(t("petReset"))}</button>
           </div>
           <div class="pet-adv-state">
             <span class="pet-thumb sm" id="petsheet-thumb" aria-hidden="true"></span>
             <span class="setting-hint" id="petsheet-note">${esc(t("petAdvanceHint"))}</span>
           </div>
         </div>`,
      )}</div>`;

    const preview = document.getElementById("pet-preview") as HTMLCanvasElement | null;

    /** The appearance that should currently be shown (same alpha slicing as the overlay). */
    const effectiveImage = async (): Promise<{ img: HTMLImageElement; frames: PackFrame[][] } | null> => {
      if (getSettings().petPack) {
        const pack = await loadPack(getSettings().petPack);
        if (pack) return { img: pack.image, frames: pack.rows };
      }
      if (getSettings().petSheet) {
        const img = await loadImage(getSettings().petSheet).catch(() => null);
        if (img) {
          const rows = sliceSheet(img);
          return {
            img,
            frames: rows.length > 0 ? rows : [[{ x: 0, y: 0, w: img.naturalWidth, h: img.naturalHeight }]],
          };
        }
      }
      const logo = await loadImage("/pet-logo.png").catch(() => null);
      return logo
        ? { img: logo, frames: [[{ x: 0, y: 0, w: logo.naturalWidth, h: logo.naturalHeight }]] }
        : null;
    };

    /** Draw the preview: same appearance, same state row, size follows the slider (100% = 72% of the stage). */
    const paintPetPreview = async (): Promise<void> => {
      if (!preview) return;
      const ctx = preview.getContext("2d");
      if (!ctx) return;
      ctx.clearRect(0, 0, preview.width, preview.height);
      const box = preview.width;
      const want = box * 0.72 * (getSettings().petSize / 100);
      // 3D current pack: offscreen-render one frame of the matching state into the preview
      if (getSettings().petPack) {
        const meta = (await listPacks().catch(() => [])).find((p) => p.id === getSettings().petPack);
        if (meta?.kind === "3d") {
          const m = await loadModelPack(getSettings().petPack).catch(() => null);
          if (m) {
            const url = await renderModelSnapshot(
              m.data,
              m.meta.clips ?? {},
              Math.round(box),
              petPreviewMood,
            ).catch(() => "");
            const img = url ? await loadImage(url).catch(() => null) : null;
            if (img) {
              ctx.drawImage(img, (box - want) / 2, (box - want) / 2, want, want);
              return;
            }
          }
        }
      }
      const eff = await effectiveImage();
      if (!eff) return;
      const rows = eff.frames;
      const row = rows[MOOD_ROWS[petPreviewMood] % rows.length] ?? rows[0];
      const f = row?.[0];
      if (!f) return;
      const k = Math.min(want / f.w, want / f.h);
      ctx.drawImage(
        eff.img,
        f.x,
        f.y,
        f.w,
        f.h,
        (box - f.w * k) / 2,
        (box - f.h * k) / 2,
        f.w * k,
        f.h * k,
      );
    };

    /** The 'in use' name + card selected state + preview all come from the same source. */
    const refreshPetUi = async (): Promise<void> => {
      const eff = effectiveSource();
      const el = document.getElementById("pet-effective");
      if (el) {
        if (eff === "pack") {
          const packs = await listPacks().catch(() => [] as Awaited<ReturnType<typeof listPacks>>);
          el.textContent =
            packs.find((p) => p.id === getSettings().petPack)?.displayName ?? getSettings().petPack;
        } else {
          el.textContent = eff === "sheet" ? t("petSourceSheet") : t("petSourceLogo");
        }
      }
      document.querySelectorAll<HTMLElement>(".pet-card").forEach((card) => {
        card.classList.toggle("active", (card.dataset.pack ?? "") === (getSettings().petPack ?? ""));
      });
      // The priority is implicit, so it must be stated explicitly: who is in use / who is overridden
      const packFlag = document.getElementById("pet-flag-pack");
      if (packFlag) packFlag.innerHTML = flagHtml(eff === "pack", false);
      const sheetFlag = document.getElementById("pet-flag-sheet");
      if (sheetFlag) {
        sheetFlag.innerHTML = flagHtml(eff === "sheet", eff === "pack" && !!getSettings().petSheet);
      }
      await paintPetPreview();
      await paintSheetState();
    };

    /** The URL row's own state: thumbnail (proof the address really works as an image) + failure reason. */
    const paintSheetState = async (): Promise<void> => {
      const thumbEl = document.getElementById("petsheet-thumb");
      const noteEl = document.getElementById("petsheet-note");
      if (!thumbEl) return;
      thumbEl.replaceChildren();
      if (!getSettings().petSheet) {
        if (noteEl) noteEl.textContent = t("petAdvanceHint");
        return;
      }
      try {
        const img = await loadImage(getSettings().petSheet);
        const rows = sliceSheet(img);
        const frames = rows.length > 0 ? rows[0] : [];
        const f = frames[0] ?? { x: 0, y: 0, w: img.naturalWidth, h: img.naturalHeight };
        const c = document.createElement("canvas");
        c.width = 72;
        c.height = 72;
        const cx = c.getContext("2d");
        if (cx) {
          const k = Math.min(c.width / f.w, c.height / f.h);
          cx.drawImage(
            img,
            f.x,
            f.y,
            f.w,
            f.h,
            (c.width - f.w * k) / 2,
            (c.height - f.h * k) / 2,
            f.w * k,
            f.h * k,
          );
          thumbEl.appendChild(c);
        }
        if (noteEl) noteEl.textContent = t("petAdvanceHint");
      } catch {
        // Can't load: say so clearly, don't make the user guess at a URL that 'does nothing'
        if (noteEl) noteEl.textContent = t("petUrlErrLoad");
      }
    };

    /** Rust returns `code[:detail]`; rendered here per language. */
    const urlErrText = (raw: unknown): string => {
      const s = String(raw ?? "");
      const map: Array<[string, I18nKey]> = [
        ["badUrl", "petUrlErrBadUrl"],
        ["network", "petUrlErrNetwork"],
        ["tooLarge:", "petUrlErrTooLarge"],
        ["http:", "petUrlErrHttp"],
        ["notImage:", "petUrlErrNotImage"],
      ];
      for (const [marker, key] of map) {
        const at = s.indexOf(marker);
        if (at < 0) continue;
        const detail = s.slice(at + marker.length).trim() || "?";
        return t(key).replace("{detail}", detail);
      }
      return s;
    };

    // NOTE: what is passed here is an 'image URL'; <img> is generated uniformly by cardHtml —
    // previously a data URL was inserted into innerHTML as HTML, so only the built-in logo showed.
    const lightLogoThumb = "/pet-logo.png";
    const cardHtml = (
      id: string,
      name: string,
      thumb: string,
      active: boolean,
      broken = false,
      kind = "2d",
    ): string =>
      `<div class="pet-card-wrap">
         <button class="pet-card${active ? " active" : ""}${broken ? " broken" : ""}" data-pack="${esc(
           id,
         )}" type="button" title="${esc(broken ? `${name} — ${t("petPackBroken")}` : name)}">
           <span class="pet-thumb">${
             thumb ? `<img src="${esc(thumb)}" alt="" />` : ""
           }${kind === "3d" ? `<span class="pet-kind">3D</span>` : ""}</span>
           <span class="pet-card-name">${esc(name)}</span>
         </button>
         <span class="pet-badge pet-badge-check" aria-hidden="true">${broken ? "!" : "✓"}</span>
         ${
           id
             ? `<button class="pet-del" data-del="${esc(id)}" type="button" title="${esc(t("petPackDelete"))}">✕</button>`
             : ""
         }
       </div>`;

    // Thumbnail = first spritesheet frame drawn into a canvas (choose a pet by sight, not by name); cached by id
    const thumbs = petThumbCache;
    /** Thumbnail + whether it can be shown. Packs that fail to render are also cached (as broken) to avoid repeated retries. */
    const packThumb = async (id: string): Promise<{ url: string; broken: boolean }> => {
      const hit = thumbs.get(id);
      if (hit) return hit;
      const broken = { url: "", broken: true };
      // 3D pack: offscreen WebGL render one frame as the thumbnail (same framing as the pet window)
      const meta = (await listPacks().catch(() => [])).find((p) => p.id === id);
      if (meta?.kind === "3d") {
        const m = await loadModelPack(id).catch(() => null);
        if (!m) {
          thumbs.set(id, broken);
          return broken;
        }
        const url = await renderModelSnapshot(m.data, m.meta.clips ?? {}, 88, "idle").catch(
          () => "",
        );
        const out = { url, broken: !url };
        thumbs.set(id, out);
        return out;
      }
      const pack = await loadPack(id).catch(() => null);
      const f = pack?.rows[0]?.[0];
      if (!pack || !f) {
        thumbs.set(id, broken);
        return broken;
      }
      const c = document.createElement("canvas");
      c.width = 88;
      c.height = 88;
      const cx = c.getContext("2d");
      if (!cx) {
        thumbs.set(id, broken);
        return broken;
      }
      const k = Math.min(c.width / f.w, c.height / f.h);
      cx.drawImage(
        pack.image,
        f.x,
        f.y,
        f.w,
        f.h,
        (c.width - f.w * k) / 2,
        (c.height - f.h * k) / 2,
        f.w * k,
        f.h * k,
      );
      const out = { url: c.toDataURL(), broken: false };
      thumbs.set(id, out);
      return out;
    };

    const renderPacks = async (): Promise<void> => {
      const grid = document.getElementById("petpack-grid");
      if (!grid) return;
      const packs = await listPacks().catch(() => [] as Awaited<ReturnType<typeof listPacks>>);
      const cur = getSettings().petPack ?? "";
      const cards = [cardHtml("", t("petSourceLogo"), lightLogoThumb, !cur)];
      for (const p of packs) {
        const thumb = await packThumb(p.id);
        cards.push(cardHtml(p.id, p.displayName, thumb.url, p.id === cur, thumb.broken, p.kind));
      }
      grid.innerHTML = cards.join("");
      const empty = document.getElementById("petpack-empty");
      if (empty) empty.hidden = packs.length > 0;

      grid.querySelectorAll<HTMLButtonElement>("button[data-pack]").forEach((b) => {
        b.addEventListener("click", () => {
          setSettings({ ...getSettings(), petPack: b.dataset.pack ?? "" });
          void save();
          void refreshPetUi();
        });
      });
      grid.querySelectorAll<HTMLButtonElement>("button[data-del]").forEach((b) => {
        b.addEventListener("click", async (ev) => {
          ev.stopPropagation();
          const id = b.dataset.del ?? "";
          // Exit animation: let it 'walk' out before removing (150ms, matching the CSS transition)
          b.closest(".pet-card-wrap")?.classList.add("removing");
          await new Promise((r) => setTimeout(r, 150));
          await deletePack(id).catch(() => undefined);
          forgetPack(id);
          thumbs.delete(id);
          if (getSettings().petPack === id) {
            setSettings({ ...getSettings(), petPack: "" });
            void save();
          }
          await renderPacks();
          await refreshPetUi();
        });
      });
    };

    const r = document.getElementById("petsize") as HTMLInputElement | null;
    r?.addEventListener("input", () => {
      setSettings({ ...getSettings(), petSize: Number(r.value) });
      document.getElementById("petsize-v")!.textContent = `${r.value}%`;
      void save();
      void paintPetPreview();
    });
    const sheet = document.getElementById("petsheet") as HTMLInputElement | null;
    sheet?.addEventListener("change", () => {
      setSettings({ ...getSettings(), petSheet: sheet.value.trim() });
      void save();
      void refreshPetUi();
    });
    document.getElementById("petreset")?.addEventListener("click", () => {
      setSettings({ ...getSettings(), petSheet: "" });
      if (sheet) sheet.value = "";
      void save();
      void refreshPetUi();
    });
    document.getElementById("petimport-url")?.addEventListener("click", async () => {
      // Download in Rust: the WebView can't read pixels of cross-origin images (can't slice or thumbnail them),
      // and the Rust side can validate content-type first — pasting a web page URL gives a clear error.
      const input = document.getElementById("petsheet") as HTMLInputElement | null;
      const url = (input?.value ?? getSettings().petSheet).trim();
      const noteEl = document.getElementById("petsheet-note");
      if (!url) return;
      if (noteEl) noteEl.textContent = `${t("petImportFromUrl")}…`;
      try {
        const id = await invoke<string>("import_pet_pack_from_url", { url, name: null });
        forgetPack(id);
        thumbs.delete(id);
        // Already saved locally: clear the URL to avoid confusion over 'which wins, URL or pack'
        setSettings({ ...getSettings(), petSheet: "", petPack: id });
        void save();
        await renderPacks();
        await refreshPetUi();
      } catch (err) {
        if (noteEl) noteEl.textContent = urlErrText(err);
        if (getSettings().petSheet === url) await paintSheetState();
      }
    });
    body.querySelectorAll<HTMLButtonElement>(".pet-mood").forEach((b) => {
      b.addEventListener("click", () => {
        petPreviewMood = (b.dataset.mood ?? "idle") as PetMood;
        body.querySelectorAll(".pet-mood").forEach((x) => {
          const on = x === b;
          x.classList.toggle("active", on);
          x.setAttribute("aria-pressed", String(on));
        });
        void paintPetPreview();
      });
    });
    document.getElementById("petpack-import")?.addEventListener("click", async () => {
      const picked = await open({
        directory: true,
        multiple: false,
        title: t("petPackImport"),
      }).catch(() => null);
      if (typeof picked !== "string") return;
      const id = await importPack(picked).catch(() => null);
      if (id) forgetPack(id);
      await renderPacks();
      await refreshPetUi();
    });
    document.getElementById("petpack-import-image")?.addEventListener("click", async () => {
      // Local image = a single-image petpack: a real file path (no base64 written into settings),
      // the whole image is drawn as one (the old upload path treated it as an 8x9 spritesheet and cut it into transparent fragments).
      const picked = await open({
        multiple: false,
        title: t("petPackImportImage"),
        filters: [{ name: "Image", extensions: ["png", "webp"] }],
      }).catch(() => null);
      if (typeof picked !== "string") return;
      const id = await importPack(picked).catch(() => null);
      if (!id) return;
      forgetPack(id);
      thumbs.delete(id);
      setSettings({ ...getSettings(), petPack: id });
      void save();
      await renderPacks();
      await refreshPetUi();
    });
    document.getElementById("petpack-import-model")?.addEventListener("click", async () => {
      // 3D petpack: a single .glb/.gltf. When the glTF references external .bin/textures, use 'Import petpack' and select the whole directory.
      const picked = await open({
        multiple: false,
        title: t("petPackImportModel"),
        filters: [{ name: "glTF / VRM", extensions: ["glb", "gltf", "vrm"] }],
      }).catch(() => null);
      if (typeof picked !== "string") return;
      const id = await importPack(picked).catch(() => null);
      if (!id) return;
      forgetPack(id);
      forgetModelPack(id);
      thumbs.delete(id);
      setSettings({ ...getSettings(), petPack: id });
      void save();
      await renderPacks();
      await refreshPetUi();
    });
    void renderPacks().then(() => refreshPetUi());


  } else if (getTab().startsWith("plugin:")) {
    // Plugin settings page: declared settings[] -> form; not declared -> config editor.
    // Back appears only when entered from the plugin detail card — the sidebar section is itself the entry point.
    // The hero establishes identity first (letter avatar + name + id + form/JSON badges); the body fills in when it arrives asynchronously.
    const id = getTab().slice("plugin:".length);
    const name = getPluginNameById().get(id) ?? id;
    const monogram = (name.trim().charAt(0) || id.charAt(0) || "?").toUpperCase();
    const back = getPluginPageReturn()
      ? `<button class="btn ghost plugin-detail-back plug-back" data-plugin-page-back type="button">‹ ${esc(t("pluginDetailBack"))}</button>`
      : "";
    body.innerHTML = `<div class="plugin-page-wrap">
      <div class="plug-hero">
        ${back}
        <div class="plug-hero-main">
          <span class="plug-avatar" aria-hidden="true">${esc(monogram)}</span>
          <div class="plug-hero-info">
            <span class="plug-hero-name">${esc(name)}</span>
            <span class="plug-hero-id">${esc(id)}</span>
          </div>
          <span class="plug-hero-badge"></span>
        </div>
      </div>
      <div class="plugin-page-list" id="plugin-page-body"></div>
    </div>`;
    body.querySelector("[data-plugin-page-back]")?.addEventListener("click", () => {
      const to = getPluginPageReturn();
      if (to) switchTab(to); // switchTab clears pluginPageReturn itself
    });
    void renderPluginPageSettings(id);
  } else if (getTab() === "alerting") {
    // hero (enable dot + title + status slot) + section cards: the same depth language as the metrics/sla/plugin settings pages,
    // no settings-list wrapper (cards carry their own ring shadow; nesting looks dirty).
    body.innerHTML = `<div class="alerting-page">
      <header class="alerting-hero"><span class="alerting-dot" id="alerting-hero-dot" aria-hidden="true"></span><div class="alerting-hero-info"><span class="alerting-hero-title">${esc(t("tabAlerting"))}</span><span class="setting-hint" id="alerting-msg"></span></div></header>
      <div class="alerting-sect"><p class="alerting-sect-title">${esc(t("alertingHint"))}</p><div id="alerting-form"></div><div class="alerting-actions"><button class="btn" id="alerting-save" type="button">${esc(t("alertingSave"))}</button><button class="btn ghost" id="alerting-test" type="button">${esc(t("alertingTest"))}</button></div></div>
       <div class="alerting-sect"><p class="alerting-sect-title">${esc(t("alertingEndpointsHint"))}</p><div class="alerting-failed-head"><button class="btn" id="alerting-ep-add" type="button">${esc(t("alertingEndpointAdd"))}</button><button class="btn ghost" id="alerting-bundle-export" type="button" title="${esc(t("alertingBundleExportTitle"))}">${esc(t("alertingBundleExport"))}</button><button class="btn ghost" id="alerting-bundle-import" type="button" title="${esc(t("alertingBundleImportTitle"))}">${esc(t("alertingBundleImport"))}</button><button class="btn ghost" id="alerting-bundle-rotate" type="button" title="${esc(t("alertingBundleRotateTitle"))}">${esc(t("alertingBundleRotate"))}</button><span class="setting-hint" id="alerting-bundle-msg"></span></div><div id="alerting-endpoints-list"></div></div>
      <div class="alerting-sect alerting-preview-section"><p class="alerting-sect-title">${esc(t("alertingSeverityPreviewTitle"))}</p><span class="setting-hint">${esc(t("alertingSeverityPreviewHint"))}</span><div class="alerting-preview-controls"><input type="text" id="alerting-preview-source" class="logs-input" placeholder="${esc(t("alertingSeverityPreviewSource"))}" style="min-width:240px" /><select id="alerting-preview-endpoint" class="logs-input"><option value="">${esc(t("alertingSeverityPreviewAllEndpoints"))}</option></select><button id="alerting-preview-predict" class="btn" type="button">${esc(t("alertingSeverityPreviewButton"))}</button><span class="setting-hint" id="alerting-preview-msg"></span></div><div id="alerting-preview-result"></div></div>
       <div class="alerting-sect"><p class="alerting-sect-title">${esc(t("alertingDispatchSimulateTitle"))}</p><span class="setting-hint">${esc(t("alertingDispatchSimulateHint"))}</span><div class="alerting-simulation"><div class="alerting-sim-controls"><input type="text" id="alerting-sim-source" class="logs-input" placeholder="${esc(t("alertingDispatchSimulateSource"))}" style="min-width:240px" /><textarea id="alerting-sim-payload" class="logs-input alerting-sim-payload" rows="2" placeholder="${esc(t("alertingDispatchSimulatePayload"))}"></textarea><button id="alerting-sim-btn" class="btn" type="button">${esc(t("alertingDispatchSimulateButton"))}</button><span class="setting-hint" id="alerting-sim-msg"></span></div><div id="alerting-sim-result"></div></div></div>
       <div class="alerting-sect"><p class="alerting-sect-title">${esc(t("alertingRetryConfig"))}</p><div id="alerting-retry-form"></div><div class="alerting-actions"><button class="btn" id="alerting-retry-save" type="button">${esc(t("alertingSave"))}</button><span class="setting-hint" id="alerting-retry-msg"></span></div></div>
       <div class="alerting-sect"><p class="alerting-sect-title">${esc(t("alertingFailedTitle"))}</p><div class="alerting-failed-head"><select class="logs-input" id="alerting-failed-state"><option value="">${esc(t("alertingFailedStateAll"))}</option><option value="pending">${esc(t("alertingFailedStatePending"))}</option><option value="exhausted">${esc(t("alertingFailedStateExhausted"))}</option></select><button class="btn ghost" id="alerting-failed-refresh" type="button">${esc(t("alertingFailedRefresh"))}</button><button class="btn ghost" id="alerting-failed-clear" type="button">${esc(t("alertingClearExhausted"))}</button><span class="setting-hint" id="alerting-failed-msg"></span></div><div id="alerting-failed-list"></div></div>
       <div class="alerting-sect"><p class="alerting-sect-title">${esc(t("alertingSilencesHint"))}</p><div class="alerting-failed-head"><button class="btn" id="alerting-silence-add" type="button">${esc(t("alertingSilencesAdd"))}</button><span class="setting-hint" id="alerting-silence-msg"></span></div><div id="alerting-silences-list"></div></div>
       <div class="alerting-sect"><p class="alerting-sect-title">${esc(t("alertingAcksHint"))}</p><div class="alerting-failed-head"><input type="text" class="logs-input" id="alerting-ack-pattern" placeholder="${esc(t("alertingAckPattern"))}" style="min-width:240px"/><input type="number" class="logs-input" id="alerting-ack-window" min="1" max="604800" value="3600" style="width:90px"/><button class="btn" id="alerting-ack-btn" type="button">${esc(t("alertingAck"))}</button><span class="setting-hint" id="alerting-ack-msg"></span></div><div id="alerting-acks-list"></div></div>
       <div class="alerting-sect"><p class="alerting-sect-title">${esc(t("alertingRecipientsHint"))}</p><div class="alerting-failed-head"><button class="btn" id="alerting-recipient-add" type="button" title="${esc(t("alertingRecipientAddTitle"))}">${esc(t("alertingRecipientAdd"))}</button><span class="setting-hint" id="alerting-recipients-msg"></span></div><div id="alerting-recipients-list"></div></div>
       <div class="alerting-sect"><p class="alerting-sect-title">${esc(t("alertingRoutesHint"))}</p><div class="alerting-failed-head"><button class="btn" id="alerting-route-add" type="button">${esc(t("alertingRouteAdd"))}</button><button class="btn ghost" id="alerting-route-import" type="button">${esc(t("alertingRouteImport"))}</button><button class="btn ghost" id="alerting-route-export" type="button">${esc(t("alertingRouteExport"))}</button><button class="btn ghost" id="alerting-route-dryrun" type="button">${esc(t("alertingRouteDryRun"))}</button><span class="setting-hint" id="alerting-route-msg"></span></div><div id="alerting-routes-list"></div></div>
       <div class="alerting-sect"><p class="alerting-sect-title">${esc(t("alertingSeverityHintsHint"))}</p><div class="alerting-failed-head"><input type="text" class="logs-input" id="severity-hint-source" placeholder="${esc(t("alertingSeverityHintSource"))}" style="min-width:240px"/><select class="logs-input" id="severity-hint-severity"><option value="info">info</option><option value="warn">warn</option><option value="error">error</option><option value="critical">critical</option></select><button class="btn" id="severity-hint-add" type="button">${esc(t("alertingSeverityHintsAdd"))}</button><button class="btn ghost" id="severity-hints-clear" type="button">${esc(t("alertingSeverityHintsClear"))}</button><span class="setting-hint" id="severity-hints-msg"></span></div><div class="setting-hint setting-hint-propagation-banner">${esc(t("alertingSeverityPropagationBanner"))}</div><div id="alerting-severity-hints-list"></div><div id="alerting-severity-chain-result"></div><div id="alerting-severity-propagation-result"></div><div id="alerting-severity-cascade-result"></div></div>
       <div class="alerting-sect"><p class="alerting-sect-title">${esc(t("alertingAggregationsHint"))}</p><div class="alerting-failed-head"><input type="text" class="logs-input" id="agg-name" placeholder="${esc(t("alertingAggregationName"))}" style="min-width:160px"/><input type="text" class="logs-input" id="agg-pattern" placeholder="${esc(t("alertingAggregationPattern"))}" value="*" style="min-width:140px"/><input type="number" class="logs-input" id="agg-window" min="1" max="86400" value="60" style="width:72px" title="${esc(t("alertingAggregationWindow"))}"/><input type="number" class="logs-input" id="agg-threshold" min="1" max="10000" value="3" style="width:64px" title="${esc(t("alertingAggregationThreshold"))}"/><select class="logs-input" id="agg-action"><option value="suppress">suppress</option><option value="downgrade">downgrade</option><option value="merge">merge</option></select><select class="logs-input" id="agg-target-severity" title="${esc(t("alertingAggregationTargetSeverity"))}"><option value="info">info</option><option value="warn">warn</option><option value="error">error</option><option value="critical">critical</option></select><button class="btn" id="agg-add" type="button">${esc(t("alertingAggregationAdd"))}</button><button class="btn ghost" id="agg-clear" type="button">${esc(t("alertingAggregationClear"))}</button><span class="setting-hint" id="agg-msg"></span></div><div id="alerting-aggregations-list"></div></div>
       <div class="alerting-sect"><p class="alerting-sect-title">${esc(t("alertingCorrelationsHint"))}</p><div class="alerting-failed-head"><input type="text" class="logs-input" id="corr-name" placeholder="${esc(t("alertingCorrelationName"))}" style="min-width:160px"/><input type="text" class="logs-input" id="corr-pattern-a" placeholder="${esc(t("alertingCorrelationPatternA"))}" style="min-width:160px"/><span class="setting-hint">→</span><input type="text" class="logs-input" id="corr-pattern-b" placeholder="${esc(t("alertingCorrelationPatternB"))}" style="min-width:160px"/><input type="number" class="logs-input" id="corr-window" min="1" max="86400" value="30" style="width:72px" title="${esc(t("alertingCorrelationWindow"))}"/><button class="btn" id="corr-add" type="button">${esc(t("alertingCorrelationAdd"))}</button><button class="btn ghost" id="corr-clear" type="button">${esc(t("alertingCorrelationClear"))}</button><span class="setting-hint" id="corr-msg"></span></div><div id="alerting-correlations-list"></div></div>
       <div class="alerting-sect"><p class="alerting-sect-title">${esc(t("alertingEscalationsHint"))}</p><div class="alerting-failed-head"><input type="text" class="logs-input" id="esc-name" placeholder="${esc(t("alertingEscalationName"))}" style="min-width:160px"/><input type="text" class="logs-input" id="esc-pattern" placeholder="${esc(t("alertingEscalationPattern"))}" value="*" style="min-width:140px"/><input type="number" class="logs-input" id="esc-after" min="1" max="86400" value="60" style="width:72px" title="${esc(t("alertingEscalationAfter"))}"/><select class="logs-input" id="esc-target-severity" title="${esc(t("alertingEscalationTargetSeverity"))}"><option value="info">info</option><option value="warn">warn</option><option value="error">error</option><option value="critical">critical</option></select><input type="text" class="logs-input" id="esc-endpoints" placeholder="${esc(t("alertingEscalationEndpoints"))}" style="min-width:200px"/><button class="btn" id="esc-add" type="button">${esc(t("alertingEscalationAdd"))}</button><button class="btn ghost" id="esc-clear" type="button">${esc(t("alertingEscalationClear"))}</button><span class="setting-hint" id="esc-msg"></span></div><div id="alerting-escalations-list"></div></div>
       <div class="alerting-sect"><p class="alerting-sect-title">${esc(t("alertingCorrelationSectionTitle"))}</p><div class="alerting-failed-head"><button class="btn" id="alerting-correlation-cycles" type="button">${esc(t("alertingCorrelationDetectCycles"))}</button><button class="btn ghost" id="alerting-correlation-refresh" type="button">${esc(t("alertingCorrelationRecentEvents"))}</button><span class="setting-hint" id="alerting-correlation-msg"></span></div><div id="alerting-correlation-cycles-result"></div><div id="alerting-correlation-timeline"></div></div>
    </div>`;
    document.getElementById("alerting-save")?.addEventListener("click", () => void saveAlertingConfig());
    document.getElementById("alerting-test")?.addEventListener("click", () => void testAlertingWebhook());
    document.getElementById("alerting-ep-add")?.addEventListener("click", () => void onAddEndpoint());
    document.getElementById("alerting-bundle-export")?.addEventListener("click", () => void onExportAlertingBundle());
    document.getElementById("alerting-bundle-import")?.addEventListener("click", () => void onImportAlertingBundle());
    document.getElementById("alerting-bundle-rotate")?.addEventListener("click", () => void onRotateBundleSecret());
    // Phase 74: predict button — populate the endpoint select, then invoke
    populateAlertingPreviewEndpointSelect();
    document.getElementById("alerting-preview-predict")?.addEventListener("click", () => void onPreviewPredict());
    // Phase 75: full-chain dry-run simulate button
    document.getElementById("alerting-sim-btn")?.addEventListener("click", () => void onSimulateDispatch());
    document.getElementById("alerting-retry-save")?.addEventListener("click", () => void saveAlertingRetryConfig());
    document.getElementById("alerting-failed-refresh")?.addEventListener("click", () => void refreshAlertingFailed());
    document.getElementById("alerting-failed-clear")?.addEventListener("click", () => void clearAlertingResolved());
    document.getElementById("alerting-failed-state")?.addEventListener("change", () => void refreshAlertingFailed());
    document.getElementById("alerting-silence-add")?.addEventListener("click", () => void onAddSilence());
    document.getElementById("alerting-ack-btn")?.addEventListener("click", () => void onAckKind());
    document.getElementById("alerting-route-add")?.addEventListener("click", () => void onAddRoute());
    document.getElementById("alerting-route-import")?.addEventListener("click", () => void onImportRoutesYaml());
    document.getElementById("alerting-route-export")?.addEventListener("click", () => void onExportRoutesYaml());
    document.getElementById("alerting-route-dryrun")?.addEventListener("click", () => void onDryRunRoute());
    document.getElementById("alerting-recipient-add")?.addEventListener("click", () => void onAddRecipient());
    void refreshRecipients();
    document.getElementById("severity-hint-add")?.addEventListener("click", () => void onAddSeverityHint());
    document.getElementById("severity-hints-clear")?.addEventListener("click", () => void clearAllSeverityHints());
    document.getElementById("agg-add")?.addEventListener("click", () => void onAddAggregation());
    document.getElementById("agg-clear")?.addEventListener("click", () => void clearAllAggregations());
    document.getElementById("agg-action")?.addEventListener("change", () => void updateAggTargetSeverityVisibility());
    document.getElementById("corr-add")?.addEventListener("click", () => void onAddCorrelation());
    document.getElementById("corr-clear")?.addEventListener("click", () => void clearAllCorrelations());
    document.getElementById("esc-add")?.addEventListener("click", () => void onAddEscalation());
    document.getElementById("esc-clear")?.addEventListener("click", () => void clearAllEscalations());
    document.getElementById("alerting-correlation-cycles")?.addEventListener("click", () => void onDetectCycles());
    document.getElementById("alerting-correlation-refresh")?.addEventListener("click", () => void onRefreshTimeline());
    void refreshAlerting();
    void refreshAlertingRetryConfig();
    void refreshAlertingFailed();
    void refreshAlertingEndpoints();
    void refreshAlertingSilences();
    void refreshAlertingAcks();
    void refreshAlertingRoutes();
    void refreshAlertingSeverityHints();
    void refreshAlertingAggregations();
    void refreshAlertingCorrelations();
    void refreshAlertingEscalations();
  } else {
    // Fallback: an unknown tab doesn't white-screen, it only shows the version card.
    body.innerHTML =
      `<div class="settings-list"><div class="about-card"><div class="logo">${ICON_PET}</div><div><b>OpenCapX</b></div><div class="ver">${esc(t("version"))} ${esc(getAppVersion())}</div><p>${esc(t("aboutText"))}</p></div></div>`;
  }
}

function wireSoundUpload(inputId: string, resetId: string, kind: SoundKind): void {
  document.getElementById(inputId)?.addEventListener("change", (e) => {
    const file = (e.target as HTMLInputElement).files?.[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = () => setCustomSound(kind, String(reader.result ?? ""));
    reader.readAsDataURL(file);
  });
  document.getElementById(resetId)?.addEventListener("click", () => setCustomSound(kind, null));
}

async function refreshAgents(): Promise<void> {
  try {
    agents = await invoke<AgentInfo[]>("get_agents");
  } catch {
    agents = [];
  }
  const box = document.getElementById("agent-list");
  if (!box) return;
  box.innerHTML = agents
    .map(
      (a) =>
        `<div class="sess"><b>${esc(a.display_name)}</b><span class="msg">${esc(a.note ?? "")}</span><button data-kind="${esc(a.kind)}" type="button">${esc(a.installed ? t("remove") : t("install"))}</button></div>`,
    )
    .join("");
  box.querySelectorAll("button[data-kind]").forEach((b) => {
    b.addEventListener("click", async () => {
      await invoke("toggle_agent", { kind: (b as HTMLElement).dataset.kind });
      await refreshAgents();
    });
  });
}

// Phase 44 — Kill switch global disable toggle.
interface CliCommandStatus {
  supported: boolean;
  installed: boolean;
  foreign: boolean;
  target: string;
  shim: string;
}

/// Settings → General: the global `opencapx` command — a symlink into PATH installed on demand.
async function refreshCliCommand(): Promise<void> {
  const status = document.getElementById("cli-status");
  const btn = document.getElementById("cli-toggle") as HTMLButtonElement | null;
  if (!status || !btn) return;
  try {
    const s = await invoke<CliCommandStatus>("cli_command_status");
    if (!s.supported) {
      status.className = "ks-status ks-off";
      status.textContent = t("cliCommandUnsupported");
      btn.style.display = "none";
      return;
    }
    btn.style.display = "";
    btn.dataset.mode = s.installed ? "uninstall" : "install";
    btn.textContent = s.installed ? t("cliCommandUninstall") : t("cliCommandInstall");
    status.className = `ks-status ${s.installed ? "ks-on" : "ks-off"}`;
    status.textContent = s.installed ? `${t("cliCommandInstalled")} ${s.target}` : t("cliCommandNotInstalled");
  } catch (err) {
    status.className = "ks-status ks-err";
    status.textContent = `✗ ${String(err)}`;
  }
}

interface KillSwitchState {
  enabled: boolean;
  reason: string;
  setAt: number;
  setBy: string;
}

async function refreshKillSwitch(): Promise<void> {
  const status = document.getElementById("ks-status");
  if (!status) return;
  try {
    const s = await invoke<KillSwitchState>("get_kill_switch_state");
    if (s.enabled) {
      const sinceDate = new Date(s.setAt * 1000).toLocaleString();
      status.className = "ks-status ks-on";
      status.innerHTML = `${esc(t("killSwitchActive"))} · ${esc(sinceDate)}${s.reason ? " · " + esc(s.reason) : ""}`;
    } else {
      status.className = "ks-status ks-off";
      status.textContent = t("killSwitchInactive");
    }
  } catch (err) {
    status.className = "ks-status ks-err";
    status.textContent = `✗ ${String(err)}`;
  }
}

/// --safe-mode read-only state: decided at core startup, cannot be toggled while running (only a restart exits it).
async function refreshSafeMode(): Promise<void> {
  const status = document.getElementById("sm-status");
  if (!status) return;
  try {
    const active = await invoke<boolean>("get_safe_mode_state");
    status.className = `ks-status ${active ? "ks-on" : "ks-off"}`;
    status.textContent = active ? t("safeModeActive") : t("safeModeInactive");
  } catch (err) {
    status.className = "ks-status ks-err";
    status.textContent = `✗ ${String(err)}`;
  }
}

async function onEnableKillSwitch(): Promise<void> {
  const reasonEl = document.getElementById("ks-reason") as HTMLInputElement | null;
  const reason = reasonEl?.value.trim() ?? "";
  if (!window.confirm(t("killSwitchConfirmEnable"))) return;
  try {
    await invoke<KillSwitchState>("enable_kill_switch", { reason: reason || null });
    await refreshKillSwitch();
    void refreshPlugins();
  } catch (err) {
    const status = document.getElementById("ks-status");
    if (status) {
      status.className = "ks-status ks-err";
      status.textContent = `✗ ${String(err)}`;
    }
  }
}

async function onDisableKillSwitch(): Promise<void> {
  if (!window.confirm(t("killSwitchConfirmDisable"))) return;
  try {
    await invoke<KillSwitchState>("disable_kill_switch");
    await refreshKillSwitch();
  } catch (err) {
    const status = document.getElementById("ks-status");
    if (status) {
      status.className = "ks-status ks-err";
      status.textContent = `✗ ${String(err)}`;
    }
  }
}

/// Plugin settings page body (only call site: render's plugin: branch).
/// Declared settings[] -> declarative form; not declared -> KV/JSON dual-mode config editor (set_plugin_config).
/// If, by the time it returns asynchronously, the page has switched away / the node was replaced by a re-render -> give up, don't write.
async function renderPluginPageSettings(id: string): Promise<void> {
  const host = document.getElementById("plugin-page-body");
  if (!host) return;
  const live = (): boolean =>
    getTab() === `plugin:${id}` && document.getElementById("plugin-page-body") === host;
  // The hero badges are in the hero (outside host): declared count / JSON mode, filled only when data arrives
  const setBadge = (text: string): void => {
    const badge = host.closest(".plugin-page-wrap")?.querySelector(".plug-hero-badge");
    if (badge) badge.textContent = text;
  };
  let view: PluginSettingsView | null = null;
  try {
    const v = await invoke<PluginSettingsView>("list_plugin_settings", { id });
    if (v && Array.isArray(v.settings) && v.settings.length > 0) view = v;
  } catch {
    /* No declaration / command unavailable: use the config editor */
  }
  if (!live()) return;
  if (view) {
    // The form is the only write entry; raw values are kept in a read-only collapsible (so the whole object the plugin actually reads stays visible)
    setBadge(t("pluginCfgDeclaredBadge").replace("{n}", String(view.settings.length)));
    const raw = JSON.stringify(view.values ?? {}, null, 2);
    host.innerHTML = `<div class="plugin-page-body-inner">${renderDeclaredSettings(id, view)}
      <div class="plug-sect plug-raw"><details class="cfg-raw-details"><summary class="setting-hint">${esc(t("pluginConfigRawView"))}</summary><textarea class="cfg-textarea" data-page-cfg-raw readonly spellcheck="false" rows="6">${esc(raw)}</textarea></details></div></div>`;
    wireDeclaredSettings(host);
    return;
  }
  let cfg: Record<string, unknown> = {};
  try {
    cfg = (await invoke<Record<string, unknown>>("get_plugin_config", { id })) ?? {};
  } catch {
    /* Empty config */
  }
  if (!live()) return;
  setBadge(t("pluginCfgJsonBadge"));
  // No declared settings[]: per-key KV CRUD (default) + an advanced JSON mode.
  // paintEditor fully repaints host and the button rows and rewires every time — the buttons are all new nodes,
  // listeners don't accumulate, and no clone-guard is needed.
  // Clearing the whole object (Reset) is only in JSON mode: that is the 'I know what I'm doing' advanced surface, and it has
  // a confirm fallback — the per-key deletion in KV mode already covers everyday edits.
  const actionsHtml = (mode: CfgMode): string =>
    `<div class="cfg-actions"><button class="btn ghost" data-page-cfg-mode type="button">${esc(mode === "kv" ? t("cfgModeJson") : t("cfgModeKv"))}</button>${mode === "json" ? `<button class="btn ghost danger" data-page-cfg-reset type="button">${esc(t("pluginConfigReset"))}</button>` : ""}<button class="btn" data-page-cfg-save type="button">${esc(t("pluginConfigSave"))}</button><span class="setting-hint" data-page-cfg-msg></span></div>`;
  const paintEditor = (mode: CfgMode, current: Record<string, unknown>): void => {
    host.innerHTML = `<div class="plugin-page-body-inner">${renderPageCfgBody(current, mode)}${actionsHtml(mode)}</div>`;
    wirePageCfgEditor(id, host, mode);
    host.querySelector("button[data-page-cfg-mode]")?.addEventListener("click", async () => {
      let latest: Record<string, unknown> = current; // switching modes uses the last persisted value, dropping unsaved edits
      try {
        latest = (await invoke<Record<string, unknown>>("get_plugin_config", { id })) ?? {};
      } catch {
        /* Keep current */
      }
      if (!live()) return; // switched away during the async period
      paintEditor(mode === "kv" ? "json" : "kv", latest);
    });
    // Clear config (visible in JSON mode only): after confirm, delete_plugin_config and repaint back to an empty KV object
    host.querySelector("button[data-page-cfg-reset]")?.addEventListener("click", async () => {
      if (!window.confirm(t("pluginConfigResetConfirm"))) return;
      try {
        await invoke("delete_plugin_config", { id });
      } catch (err) {
        const m = host.querySelector<HTMLElement>("[data-page-cfg-msg]");
        if (m) {
          m.textContent = `${t("pluginConfigSaveFailed")}: ${(err as Error).message ?? err}`;
          m.classList.add("cfg-err");
        }
        return;
      }
      paintEditor("kv", {});
      const m = host.querySelector<HTMLElement>("[data-page-cfg-msg]");
      if (m) m.textContent = t("pluginConfigResetDone");
    });
  };
  paintEditor("kv", cfg);
}

/// One KV editor row: key is read-only (renaming = delete and re-add, avoiding accidental damage to keys the plugin reads); value is editable.
/// Every row stores the original JSON in data-orig-json — if the value was untouched it is written back as-is, without a text->scalar round trip
/// (otherwise a string-shaped value like '1234' would be stored as a number). Non-scalars (object/array/null) are read-only;
/// to change the structure, switch to JSON mode.
function kvRowHtml(key: string, value: unknown): string {
  const complex = typeof value === "object"; // null is also a non-scalar: read-only + write back the original JSON
  const shown = escAttr(complex ? JSON.stringify(value) : String(value));
  // hover can only report the facts it has (key + value type): this plugin declares no schema, so the host knows no more semantics
  const typeName = value === null ? "null" : Array.isArray(value) ? "array" : typeof value;
  return `<div class="kv-row" title="${escAttr(`${key}: ${typeName}`)}"><input class="kv-key" value="${escAttr(key)}" readonly/><input class="kv-val" value="${shown}"${complex ? " readonly" : ""} data-orig-json="${escAttr(JSON.stringify(value))}"/><button class="btn ghost danger" data-kv-del type="button" title="${escAttr(t("remove"))}" aria-label="${escAttr(t("remove"))}">×</button></div>`;
}

/// Input string -> scalar: true/false/numbers stored by type; quoted -> forced string; everything else a plain string.
function coerceScalar(raw: string): unknown {
  const s = raw.trim();
  if (s === "true") return true;
  if (s === "false") return false;
  if (s.length >= 2 && s.startsWith('"') && s.endsWith('"')) return s.slice(1, -1);
  if (/^-?\d+(\.\d+)?$/.test(s)) {
    const n = Number(s);
    if (String(n) === s) return n;
  }
  return raw;
}

/// KV editor body: a row per existing key + an 'Add key' row. The whole block sits in a .plug-sect card, using the same depth language as declarative settings.
function renderKVEditor(cfg: Record<string, unknown>): string {
  const rows = Object.entries(cfg).map(([k, v]) => kvRowHtml(k, v)).join("");
  return `<div class="plug-sect plug-kv-sect"><p class="kv-undeclared-hint">${esc(t("pluginCfgUndeclaredHint"))}</p><div class="kv-editor">${rows}
    <div class="kv-row kv-add"><input class="kv-key" data-kv-new-key type="text" placeholder="${escAttr(t("cfgKVKeyPlaceholder"))}"/><input class="kv-val" data-kv-new-val type="text" placeholder="${escAttr(t("cfgKVValuePlaceholder"))}"/><button class="btn ghost" data-kv-add type="button" title="${escAttr(t("cfgKVAdd"))}" aria-label="${escAttr(t("cfgKVAdd"))}">+</button></div></div></div>`;
}

type CfgMode = "kv" | "json";

/// Config editor body for plugins without declarations. KV mode by default (per-key CRUD);
/// 'Advanced: JSON' switches to a whole-object textarea (fallback for nested structures); both modes use the same persist command.
function renderPageCfgBody(cfg: Record<string, unknown>, mode: CfgMode): string {
  if (mode === "json") {
    return `<div class="plug-sect plug-json-sect"><textarea class="cfg-textarea" data-page-cfg-textarea spellcheck="false" rows="6">${esc(JSON.stringify(cfg, null, 2))}</textarea></div>`;
  }
  return renderKVEditor(cfg);
}

/// host = #plugin-page-body (fully repainted on every paintEditor; buttons are all new nodes, so listeners don't accumulate).
function wirePageCfgEditor(id: string, host: HTMLElement, mode: CfgMode): void {
  const msg = host.querySelector<HTMLElement>("[data-page-cfg-msg]");
  if (!msg) return;
  const fail = (text: string): void => {
    msg.textContent = text;
    msg.classList.add("cfg-err");
  };
  const clearMsg = (): void => {
    msg.textContent = "";
    msg.classList.remove("cfg-err");
  };
  if (mode === "json") {
    // JSON mode: the original textarea semantics (parse validation + set_plugin_config)
    host.querySelector("button[data-page-cfg-save]")?.addEventListener("click", async () => {
      const ta = host.querySelector<HTMLTextAreaElement>("textarea[data-page-cfg-textarea]");
      if (!ta) return;
      let parsed: unknown;
      try {
        parsed = JSON.parse(ta.value);
      } catch (err) {
        fail(`${t("pluginConfigInvalid")}: ${(err as Error).message}`);
        return;
      }
      if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
        fail(t("pluginConfigInvalid"));
        return;
      }
      try {
        await invoke("set_plugin_config", { id, value: parsed });
        msg.textContent = t("pluginConfigSaved");
        msg.classList.remove("cfg-err");
      } catch (err) {
        fail(`${t("pluginConfigSaveFailed")}: ${(err as Error).message ?? err}`);
      }
    });
    return;
  }
  // KV mode: read rows -> object; key validation + dedupe; rows whose value was untouched (the value still equals the painted
  // defaultValue) are written back to data-orig-json as-is; only rows the user changed and newly added rows go through coerceScalar's
  // text->scalar rules.
  // out has no prototype: a key can be named __proto__ (whatever a plugin writes via core::config::set is what it is),
  // on a plain object literal out["__proto__"] = v only changes the prototype and the key is silently dropped.
  const readObject = (): Record<string, unknown> | null => {
    const out = Object.create(null) as Record<string, unknown>;
    for (const row of host.querySelectorAll<HTMLDivElement>(".kv-row:not(.kv-add)")) {
      const keyEl = row.querySelector<HTMLInputElement>(".kv-key");
      const valEl = row.querySelector<HTMLInputElement>(".kv-val");
      if (!keyEl || !valEl) continue;
      const key = keyEl.readOnly ? keyEl.value : keyEl.value.trim(); // keys stored in the library are carried over as-is (leading/trailing spaces are part of the key)
      // Validate only keys the user can change: keys stored in the library aren't restricted by character set (written by plugins via core::config::set),
      // read-only rows are written back as-is, otherwise the editor gets stuck on an unfixable error (renaming = delete and re-add).
      if (!keyEl.readOnly && !/^[A-Za-z_][A-Za-z0-9_.-]{0,63}$/.test(key)) {
        fail(`${t("cfgKVBadKey")}: ${key || t("cfgKVEmptyKey")}`);
        return null;
      }
      if (Object.prototype.hasOwnProperty.call(out, key)) {
        fail(`${t("cfgKVDupKey")}: ${key}`);
        return null;
      }
      out[key] = valEl.dataset.origJson !== undefined && !valEl.classList.contains("kv-touched") ? JSON.parse(valEl.dataset.origJson) : coerceScalar(valEl.value);
    }
    return out;
  };
  // Delete row
  host.querySelectorAll("button[data-kv-del]").forEach((b) => {
    b.addEventListener("click", () => {
      b.closest(".kv-row")?.remove();
      clearMsg();
    });
  });
  // Value rows the user touched are stored by their typed form (retyping the same text = explicitly changing the type); untouched rows are written back as-is,
  // using data-orig-json to preserve the difference between the string '1234' and the number 1234.
  host.querySelectorAll<HTMLInputElement>(".kv-val").forEach((v) => {
    v.addEventListener("input", () => v.classList.add("kv-touched"));
  });
  // Add row: validate key and value together; once valid it enters the DOM (the key is still editable), with full validation only on save
  const addRow = (): void => {
    const keyInput = host.querySelector<HTMLInputElement>("[data-kv-new-key]");
    const valInput = host.querySelector<HTMLInputElement>("[data-kv-new-val]");
    if (!keyInput || !valInput) return;
    const key = keyInput.value.trim();
    if (!/^[A-Za-z_][A-Za-z0-9_.-]{0,63}$/.test(key)) {
      fail(`${t("cfgKVBadKey")}: ${key || t("cfgKVEmptyKey")}`);
      return;
    }
    const rowHtml = `<div class="kv-row"><input class="kv-key" value="${escAttr(key)}"/><input class="kv-val" value="${escAttr(valInput.value)}"/><button class="btn ghost danger" data-kv-del type="button" title="${escAttr(t("remove"))}" aria-label="${escAttr(t("remove"))}">×</button></div>`;
    keyInput.value = "";
    valInput.value = "";
    // Insert before the 'Add key' row: otherwise the add row is no longer last and the editor reads [key][add row][new key].
    const editor = host.querySelector(".kv-editor");
    const addRowEl = editor?.querySelector(".kv-add");
    addRowEl?.insertAdjacentHTML("beforebegin", rowHtml);
    const added = addRowEl?.previousElementSibling as HTMLElement | null | undefined;
    added?.querySelector("button[data-kv-del]")?.addEventListener("click", () => {
      added.remove();
      clearMsg();
    });
    clearMsg();
  };
  host.querySelector("button[data-kv-add]")?.addEventListener("click", addRow);
  host.querySelector<HTMLInputElement>("[data-kv-new-val]")?.addEventListener("keydown", (e) => {
    if (e.key === "Enter") addRow();
  });
  // Save: content already typed in the 'Add key' row is committed first, otherwise the just-typed key/value is silently dropped while still reporting Saved
  host.querySelector("button[data-page-cfg-save]")?.addEventListener("click", async () => {
    const pending = host.querySelector<HTMLInputElement>("[data-kv-new-key]");
    if (pending && pending.value.trim() !== "") {
      addRow();
      if (pending.value.trim() !== "") return; // addRow rejected it (invalid key): the error is already shown
    }
    const obj = readObject();
    if (!obj) return;
    try {
      await invoke("set_plugin_config", { id, value: obj });
      msg.textContent = t("pluginConfigSaved");
      msg.classList.remove("cfg-err");
    } catch (err) {
      fail(`${t("pluginConfigSaveFailed")}: ${(err as Error).message ?? err}`);
    }
  });
}

/// Localization resolution for plugin text (frozen order, same as the Rust side): current language -> en -> lexicographically smallest key.
/// Do not write fallback logic elsewhere — both sides must use exactly the same order.
function resolveText(v: LocalizedText | undefined, locale: string): string {
  if (v === undefined) return "";
  if (typeof v === "string") return v;
  const direct = v[locale];
  if (direct !== undefined) return direct;
  const en = v["en"];
  if (en !== undefined) return en;
  const keys = Object.keys(v).sort();
  return keys.length > 0 ? v[keys[0]] : "";
}

/// section grouping key: compare raw declaration values (string or mapping), not resolved text.
/// Mappings are sorted by key and joined, so the same section isn't split into two headings because of key order.
function sectionKey(section: LocalizedText | undefined): string {
  if (section === undefined) return "";
  if (typeof section === "string") return `s:${section}`;
  return `m:${Object.keys(section)
    .sort()
    .map((k) => `${k}=${section[k]}`)
    .join("\u0001")}`;
}

/// Predicate evaluation (pure function, no DOM). secret values never enter values, so isSet must also check secretsSet;
/// unknown ops always return false — Rust rejects them at install time; this is only defensive and never throws.
function evalCond(cond: Cond, values: Record<string, unknown>, secretsSet: readonly string[]): boolean {
  switch (cond.op) {
    case "equals":
      return values[cond.key] === cond.value;
    case "notEquals":
      return values[cond.key] !== cond.value;
    case "in":
      return cond.values.includes(values[cond.key]);
    case "isSet":
      return secretsSet.includes(cond.key) || values[cond.key] !== undefined;
    case "all":
      return cond.conds.every((c) => evalCond(c, values, secretsSet));
    case "any":
      return cond.conds.some((c) => evalCond(c, values, secretsSet));
    case "not":
      return !evalCond(cond.cond, values, secretsSet);
    default:
      return false;
  }
}

/// Pass determination for a single rule. 'Not set' (missing / null / '' / empty array) is handled by required alone:
/// all other rules are skipped — an optional field left empty = use the plugin default, and must not be permanently failed by pattern/minLength with no way to clear it.
/// Rules whose types don't match always pass, so a bad manifest doesn't lock the whole form (already validated by Rust at install time).
function rulePasses(r: ValidateRule, value: unknown): boolean {
  const unset = value === undefined || value === null || value === "" || (Array.isArray(value) && value.length === 0);
  if (r.type !== "required" && unset) return true;
  switch (r.type) {
    case "required":
      return !(value === undefined || value === null || value === "" || (Array.isArray(value) && value.length === 0));
    case "minLength":
      return typeof value !== "string" || value.length >= r.value;
    case "maxLength":
      return typeof value !== "string" || value.length <= r.value;
    case "min":
    case "max": {
      if (value === "" || value === undefined || value === null) return true;
      const n = typeof value === "number" ? value : Number(value);
      if (!Number.isFinite(n)) return true;
      return r.type === "min" ? n >= r.value : n <= r.value;
    }
    case "pattern": {
      if (typeof value !== "string") return true;
      try {
        return new RegExp(r.regex).test(value);
      } catch {
        return true;
      }
    }
    default:
      return true;
  }
}

/// Default text (not reached when the rule carries its own message).
function defaultRuleMessage(r: ValidateRule): string {
  switch (r.type) {
    case "required":
      return t("pluginValidateRequired");
    case "minLength":
      return t("pluginValidateMinLength").replace("{n}", String(r.value));
    case "maxLength":
      return t("pluginValidateMaxLength").replace("{n}", String(r.value));
    case "min":
      return t("pluginValidateMin").replace("{n}", String(r.value));
    case "max":
      return t("pluginValidateMax").replace("{n}", String(r.value));
    case "pattern":
      return t("pluginValidatePattern");
    default:
      return t("pluginConfigInvalid");
  }
}

/// Rule evaluation: return the first failing message, or null if all pass. A rule's own message takes priority.
function evalRules(rules: ValidateRule[], value: unknown): string | null {
  for (const r of rules) {
    if (rulePasses(r, value)) continue;
    // message may be a localized mapping: resolved in resolveText's frozen order (shared by the mount check and the pre-write gate)
    return resolveText(r.message, getLocale()) || defaultRuleMessage(r);
  }
  return null;
}

/// Declaration table registered during render: pair = '<pluginId>|<key>' -> SettingDecl.
/// Before writing, look up validation rules by pair (re-renders overwrite entries, and stale entries are never referenced by the DOM).
const declaredDecls = new Map<string, SettingDecl>();

/** Control field encoding: '<pluginId>|<settingKey>' (neither id nor key contains '|') */
function splitSettingPair(pair: string): [string, string] {
  const i = pair.indexOf("|");
  return [pair.slice(0, i), pair.slice(i + 1)];
}

function renderDeclaredSettings(id: string, view: PluginSettingsView): string {
  // Group by section: one .plug-sect card per segment, with small cards inside (separated by gap, no border-bottom);
  // declarations without a section go into one untitled card (the heading appears only when section !== undefined, matching existing behavior).
  const sects: { title: string | null; rows: string[] }[] = [];
  let current: { title: string | null; rows: string[] } | null = null;
  let lastSection: string | null = null;
  // order stable sort: declared orders with smaller values first; equal and undeclared orders fall back to manifest order
  const ordered = view.settings
    .map((s, i) => ({ s, i }))
    .sort((a, b) => (a.s.order ?? Number.MAX_SAFE_INTEGER) - (b.s.order ?? Number.MAX_SAFE_INTEGER) || a.i - b.i)
    .map((x) => x.s);
  for (const s of ordered) {
    declaredDecls.set(`${id}|${s.key}`, s);
    // visible predicate is false: the whole row is not rendered (the predicate depends only on stored values and recomputes immediately after a write)
    if (s.visible && !evalCond(s.visible, view.values, view.secretsSet)) continue;
    const key = sectionKey(s.section);
    if (key !== lastSection || current === null) {
      // Consecutive declarations with the same section share a heading (grouped by raw value); the heading takes the resolved text of the segment's first item
      current = { title: s.section !== undefined ? esc(resolveText(s.section, getLocale())) : null, rows: [] };
      sects.push(current);
      lastSection = key;
    }
    current.rows.push(renderDeclaredRow(id, s, view));
  }
  const cards = sects
    .map((sec) => {
      const title = sec.title !== null ? `<p class="settings-group-title">${sec.title}</p>` : "";
      return `<div class="plug-sect">${title}${sec.rows.join("")}</div>`;
    })
    .join("");
  return `<div class="declared-settings"><p class="setting-hint">${esc(t("pluginSettingDeclared"))}</p>${cards}</div>`;
}

/// Single declaration -> control. When the disabled predicate is true the control is disabled and the whole row is dimmed;
/// on mount, run the rules once against stored/default values to flag bad values left by older plugin versions directly (hint only, non-blocking).
function renderDeclaredRow(id: string, s: SettingDecl, view: PluginSettingsView): string {
  const pair = `${id}|${s.key}`;
  const locale = getLocale();
  const label = esc(resolveText(s.label, locale) || s.key);
  const descText = resolveText(s.description, locale);
  // Deprecated: the control stays usable; the row carries a marker + reason (text is localized; an empty text is caught by Rust validation, with a generic fallback word here).
  // The badge shares the row with the label (inline), no longer pushed below the description by the flex column.
  const depText = resolveText(s.deprecated, locale) || (s.deprecated ? t("pluginSettingDeprecated") : "");
  const depBadge = s.deprecated ? `<span class="cfg-deprecated-badge">${esc(t("pluginSettingDeprecated"))}</span>` : "";
  const labelLine = `<span class="setting-label-line"><span class="setting-label">${label}</span>${depBadge}</span>`;
  const desc = (descText ? `<span class="setting-hint">${esc(descText)}</span>` : "")
    + (s.deprecated ? `<span class="setting-hint">${esc(depText)}</span>` : "");
  // Search haystack (all lowercase): resolved label/description + the raw values of every language in the mapping
  // + aliases + key + plugin id. Chinese queries match even in an English UI (thanks to the raw values in the mapping).
  const parts: string[] = [resolveText(s.label, locale), descText, s.key, id, ...(s.aliases ?? [])];
  if (typeof s.label === "object") parts.push(...Object.values(s.label));
  if (typeof s.description === "object") parts.push(...Object.values(s.description));
  for (const o of s.options ?? []) {
    if (typeof o !== "string" && o.label) parts.push(resolveText(o.label, locale) ?? "", ...Object.values(o.label));
  }
  const haystack = esc(parts.join("\n").toLowerCase()).replace(/"/g, "&quot;");
  const off = !!s.disabled && evalCond(s.disabled, view.values, view.secretsSet);
  const dis = off ? " disabled" : "";
  const cls = off ? "setting-row plug-row cfg-row-off" : "setting-row plug-row";
  const rowOpen = `<div class="${cls}" data-cfg-search="${haystack}">`;
  const rowOpenV = `<div class="setting-row vertical plug-row${off ? " cfg-row-off" : ""}" data-cfg-search="${haystack}">`;
  // secret values live in the keychain and the frontend can't get them — don't evaluate at mount (otherwise required would always false-positive)
  const stored = view.values[s.key] ?? s.default;
  const mountFail = s.type === "secret" ? null : evalRules(s.validate ?? [], stored);
  const msg = `<span class="setting-hint${mountFail ? " cfg-err" : ""}" data-set-msg="${esc(pair)}">${mountFail ? esc(mountFail) : ""}</span>`;
  const save = `<button class="btn" data-set-save="${esc(pair)}" data-set-kind="${esc(s.type)}" type="button"${dis}>${esc(t("pluginConfigSave"))}</button>`;
  // Modified marker + per-key restore default: value present and != default -> marker;
  // the restore button appears only when a default exists (keys without a default have no 'default value' to write back, only the marker).
  // secret/button/list never enter settings_view.values -> hasVal is always false, so the marker naturally never appears.
  const hasVal = view.values[s.key] !== undefined;
  const modified = hasVal && JSON.stringify(view.values[s.key]) !== JSON.stringify(s.default);
  const mod = modified ? `<span class="cfg-mod" title="${esc(t("pluginSettingModified"))}">●</span>` : "";
  const rst = modified && s.default !== undefined
    ? `<button class="btn ghost" data-set-reset-default="${esc(pair)}" type="button"${dis} title="${esc(t("pluginSettingResetDefault"))}">${esc(t("pluginSettingResetDefault"))}</button>`
    : "";
  switch (s.type) {
    case "toggle": {
      const checked = view.values[s.key] === true ? " checked" : "";
      return `${rowOpen}<div class="setting-info">${labelLine}${desc}</div><input type="checkbox" data-set-toggle="${esc(pair)}"${checked}${dis}/>${msg}${mod}${rst}</div>`;
    }
    case "dropdown": {
      const cur = String(view.values[s.key] ?? "");
      const opts = (s.options ?? [])
        .map((o) => {
          const value = typeof o === "string" ? o : o.value;
          const text = typeof o === "string" ? o : (resolveText(o.label, locale) || o.value);
          return `<option value="${esc(value)}"${value === cur ? " selected" : ""}>${esc(text)}</option>`;
        })
        .join("");
      return `${rowOpen}<div class="setting-info">${labelLine}${desc}</div><select data-set-select="${esc(pair)}"${dis}>${opts}</select>${msg}${mod}${rst}</div>`;
    }
    case "radio-group": {
      const cur = String(view.values[s.key] ?? "");
      // Reuse the data-set-select hook: it's the same 'change -> store the option string', so don't open a second save path for it
      const radios = (s.options ?? [])
        .map((o) => {
          const value = typeof o === "string" ? o : o.value;
          const text = typeof o === "string" ? o : (resolveText(o.label, locale) || o.value);
          return `<label class="cfg-radio"><input type="radio" name="${esc(pair)}" value="${esc(value)}" data-set-select="${esc(pair)}"${value === cur ? " checked" : ""}${dis}/><span>${esc(text)}</span></label>`;
        })
        .join("");
      return `${rowOpen}<div class="setting-info">${labelLine}${desc}</div><div class="cfg-radio-group">${radios}</div>${msg}${mod}${rst}</div>`;
    }
    case "color": {
      const val = typeof view.values[s.key] === "string" ? String(view.values[s.key]) : "#000000";
      return `${rowOpen}<div class="setting-info">${labelLine}${desc}</div><input type="color" data-set-input="${esc(pair)}" value="${esc(val)}"${dis}/>${save}${msg}${mod}${rst}</div>`;
    }
    case "slider": {
      const min = s.min ?? 0;
      const max = s.max ?? 100;
      const step = s.step ?? 1;
      const v = view.values[s.key];
      const val = typeof v === "number" && Number.isFinite(v) ? v : min;
      // The readout updates live while dragging (without persisting); persisting uses the same save button, and validation takes the same path
      return `${rowOpen}<div class="setting-info">${labelLine}${desc}</div><div class="cfg-slider"><input type="range" min="${esc(String(min))}" max="${esc(String(max))}" step="${esc(String(step))}" data-set-input="${esc(pair)}" value="${esc(String(val))}"${dis}/><span class="setting-hint" data-set-readout="${esc(pair)}">${esc(String(val))}</span></div>${save}${msg}${mod}${rst}</div>`;
    }
    case "secret": {
      const set = view.secretsSet.includes(s.key);
      const hint = set ? t("pluginSettingSecretSet") : t("pluginSettingSecretUnset");
      return `${rowOpen}<div class="setting-info">${labelLine}${desc}<span class="setting-hint" data-set-mask="${esc(pair)}">${esc(hint)}</span></div><input type="password" data-set-input="${esc(pair)}" placeholder="********" spellcheck="false"${dis}/>${save}${msg}${mod}${rst}</div>`;
    }
    case "button": {
      return `${rowOpen}<div class="setting-info">${labelLine}${desc}</div><button class="btn ghost" data-set-action="${esc(pair)}" type="button"${dis}>${label}</button>${msg}${mod}${rst}</div>`;
    }
    case "textarea": {
      const val = typeof view.values[s.key] === "string" ? String(view.values[s.key]) : "";
      return `${rowOpenV}<div class="setting-info">${labelLine}${desc}</div><textarea class="cfg-textarea" rows="3" spellcheck="false" data-set-input="${esc(pair)}"${dis}>${esc(val)}</textarea>${save}${msg}${mod}${rst}</div>`;
    }
    case "number": {
      const v = view.values[s.key];
      const val = typeof v === "number" ? String(v) : "";
      return `${rowOpen}<div class="setting-info">${labelLine}${desc}</div><input type="number" step="1" data-set-input="${esc(pair)}" value="${esc(val)}"${dis}/>${save}${msg}${mod}${rst}</div>`;
    }
    case "list": {
      // P3 — items belong to the plugin process: fetched at mount (op:list), row actions forwarded immediately;
      // no save button (each op makes the plugin persist and return a new array). msg still renders like other types:
      // an op failure writes an inline hint rather than firing an alert (the only modal in the settings window).
      return `${rowOpenV}<div class="setting-info">${labelLine}${desc}</div>
        <div class="kv-editor" data-list-body="${esc(pair)}"><span class="setting-hint">${esc(t("pluginSettingListLoading"))}</span></div>
        <div class="kv-row kv-add"><input class="kv-val" data-list-new type="text" placeholder="${esc(t("pluginSettingListAddPlaceholder"))}"${dis}/><button class="btn ghost" data-list-add="${esc(pair)}" type="button" title="${escAttr(t("pluginSettingListAdd"))}" aria-label="${escAttr(t("pluginSettingListAdd"))}"${dis}>+</button></div>${msg}${mod}${rst}</div>`;
    }
    default: {
      // text / path
      const val = typeof view.values[s.key] === "string" ? String(view.values[s.key]) : "";
      // pick takes effect only on path: file -> pick a file; default/other -> pick a directory (preserving existing behavior)
      const browse =
        s.type === "path"
          ? `<button class="btn ghost" data-set-browse="${esc(pair)}" data-set-pick="${esc(s.pick === "file" ? "file" : "directory")}" type="button"${dis}>${esc(t("pluginSettingBrowse"))}</button>`
          : "";
      return `${rowOpen}<div class="setting-info">${labelLine}${desc}</div><input type="text" spellcheck="false" data-set-input="${esc(pair)}" value="${esc(val)}"${dis}/>${browse}${save}${msg}${mod}${rst}</div>`;
    }
  }
}

/// Per-key restore default: write back the declaration's default — through the same saveDeclaredSetting
/// validation/hint/re-render pipeline, without opening another write path.
function wireResetDefault(box: HTMLElement, msgOf: (pair: string) => Element | null): void {
  box.querySelectorAll<HTMLButtonElement>("button[data-set-reset-default]").forEach((b) => {
    b.addEventListener("click", () => {
      const pair = b.dataset.setResetDefault ?? "";
      const [id, key] = splitSettingPair(pair);
      const decl = declaredDecls.get(pair);
      if (!decl || decl.default === undefined) return;
      void saveDeclaredSetting(id, key, decl.default, msgOf(pair));
    });
  });
}

async function saveDeclaredSetting(
  id: string,
  key: string,
  value: unknown,
  msg: Element | null
): Promise<boolean> {
  // Run validation first (rules looked up by pair): on failure hint in place, don't write or re-render — the value the user just typed must stay in the box
  const failure = evalRules(declaredDecls.get(`${id}|${key}`)?.validate ?? [], value);
  if (failure) {
    if (msg) {
      msg.textContent = failure;
      msg.classList.add("cfg-err");
    }
    return false;
  }
  try {
    await invoke("set_plugin_setting", { id, key, value });
    if (msg) {
      msg.textContent = t("pluginConfigSaved");
      msg.classList.remove("cfg-err");
    }
    // On success re-render with the new values: visible/disabled predicates recompute automatically, no manual update() needed
    await refreshPluginConfig();
    // The plugin settings page shares this data: the `if (getTab().startsWith("plugin:")) render()` inside refreshPluginConfig
    // re-renders it instead of calling the detail-page render separately
    return true;
  } catch (err) {
    if (msg) {
      msg.textContent = `${t("pluginConfigSaveFailed")}: ${(err as Error).message ?? err}`;
      msg.classList.add("cfg-err");
    }
    return false;
  }
}

function wireDeclaredSettings(box: HTMLElement): void {
  const msgOf = (pair: string): Element | null =>
    box.querySelector(`span[data-set-msg="${cssEscape(pair)}"]`);

  wireResetDefault(box, msgOf);

  box.querySelectorAll<HTMLInputElement>("input[data-set-toggle]").forEach((el) => {
    el.addEventListener("change", async () => {
      const pair = el.dataset.setToggle ?? "";
      const [id, key] = splitSettingPair(pair);
      await saveDeclaredSetting(id, key, el.checked, msgOf(pair));
    });
  });

  // dropdown and radio-group share this one 'change -> store the option string' path (radio's el.value is the selected option)
  box.querySelectorAll<HTMLSelectElement | HTMLInputElement>("select[data-set-select], input[data-set-select]").forEach((el) => {
    el.addEventListener("change", async () => {
      const pair = el.dataset.setSelect ?? "";
      const [id, key] = splitSettingPair(pair);
      await saveDeclaredSetting(id, key, el.value, msgOf(pair));
    });
  });

  // slider readout: shown live while dragging; persisting still goes through the save button
  box.querySelectorAll<HTMLInputElement>('input[type="range"][data-set-input]').forEach((el) => {
    el.addEventListener("input", () => {
      const pair = el.dataset.setInput ?? "";
      const out = box.querySelector(`span[data-set-readout="${cssEscape(pair)}"]`);
      if (out) out.textContent = el.value;
    });
  });

  box.querySelectorAll<HTMLButtonElement>("button[data-set-save]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const pair = btn.dataset.setSave ?? "";
      const kind = btn.dataset.setKind ?? "text";
      const [id, key] = splitSettingPair(pair);
      const input = box.querySelector<HTMLInputElement | HTMLTextAreaElement>(
        `[data-set-input="${cssEscape(pair)}"]`
      );
      if (!input) return;
      let value: unknown = input.value;
      if (kind === "number") {
        const n = Number.parseInt(input.value, 10);
        if (!Number.isFinite(n) || String(n) !== input.value.trim()) {
          const msg = msgOf(pair);
          if (msg) {
            msg.textContent = t("pluginConfigInvalid");
            msg.classList.add("cfg-err");
          }
          return;
        }
        value = n;
      }
      if (kind === "slider") {
        const n = Number(input.value);
        if (!Number.isFinite(n)) {
          const msg = msgOf(pair);
          if (msg) {
            msg.textContent = t("pluginConfigInvalid");
            msg.classList.add("cfg-err");
          }
          return;
        }
        value = n;
      }
      const ok = await saveDeclaredSetting(id, key, value, msgOf(pair));
      if (ok && kind === "secret") {
        input.value = "";
        const mask = box.querySelector(`span[data-set-mask="${cssEscape(pair)}"]`);
        if (mask) mask.textContent = t("pluginSettingSecretSet");
      }
    });
  });

  box.querySelectorAll<HTMLButtonElement>("button[data-set-browse]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const pair = btn.dataset.setBrowse ?? "";
      const input = box.querySelector<HTMLInputElement>(
        `[data-set-input="${cssEscape(pair)}"]`
      );
      if (!input) return;
      // pick === 'file' -> file picker; default is still directory (preserving existing behavior)
      const fileMode = btn.dataset.setPick === "file";
      try {
        const picked = await open({ directory: !fileMode, multiple: false });
        if (typeof picked === "string") input.value = picked;
      } catch {
        /* dialog unavailable */
      }
    });
  });

  box.querySelectorAll<HTMLButtonElement>("button[data-set-action]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const pair = btn.dataset.setAction ?? "";
      const [id, key] = splitSettingPair(pair);
      const msg = msgOf(pair);
      try {
        await invoke("invoke_plugin_setting_action", { id, key });
        if (msg) {
          msg.textContent = t("pluginSettingActionDone");
          msg.classList.remove("cfg-err");
        }
      } catch (err) {
        if (msg) {
          msg.textContent = `${t("pluginConfigSaveFailed")}: ${(err as Error).message ?? err}`;
          msg.classList.add("cfg-err");
        }
      }
    });
  });

  // ── P3 list control: forward op -> plugin returns a new array -> repaint that list ──────────────────
  box.querySelectorAll<HTMLElement>("[data-list-body]").forEach((bodyEl) => {
    // Host row = the .setting-row containing data-list-body
    const row = bodyEl.closest(".setting-row") as HTMLElement | null;
    if (!row) return;
    const pair = bodyEl.dataset.listBody ?? "";
    const [id, key] = splitSettingPair(pair);
    // The result only writes an inline hint: failure takes the same path as other declarative controls — a modal here would yank the user off the current page.
    // Success clears the previous error (the repainted list itself is the receipt).
    const say = (text: string, isErr: boolean): void => {
      const msg = msgOf(pair);
      if (!msg) return;
      msg.textContent = text;
      msg.classList.toggle("cfg-err", isErr);
    };
    // Don't send again until the previous send returns: the button's index is from paint time, the list has changed, and sending again would hit a different item
    // (double-clicking x quickly would delete two; repeated up/down would undo the move just made). op takes up to 10s (the plugin may be lazy-starting).
    let busy = false;
    // When it fails before the first frame is painted (plugin won't start / timeout), clear the 'Loading…' placeholder,
    // otherwise a row is stuck in the loading state forever and the page has no retry entry. Use a flag instead of probing the DOM:
    // an empty list is also a legitimate result with no .kv-row painted, and probing the DOM would wrongly clear '(empty list)'.
    let painted = false;
    const call = async (payload: Record<string, unknown>): Promise<boolean> => {
      if (busy) return false;
      busy = true;
      try {
        const r = await invoke<unknown[]>("invoke_plugin_setting_list", { id, key, ...payload });
        paint(Array.isArray(r) ? r : []);
        say("", false);
        return true;
      } catch (err) {
        if (!painted) bodyEl.innerHTML = "";
        say(`${t("pluginConfigSaveFailed")}: ${(err as Error).message ?? err}`, true);
        return false;
      } finally {
        busy = false;
      }
    };
    const paint = (items: unknown[]): void => {
      painted = true;
      // The first row's up and last row's down are always disabled: to is Option<usize>, and to=-1 fails to deserialize,
      // to=len is a silent no-op on the plugin side — out-of-bounds moves are never sent. When the row is turned off by the disable predicate, all are disabled.
      const rowOff = row.classList.contains("cfg-row-off");
      // aria-label: up / down / x read out alone have no semantics (x is read as 'multiplication sign').
      bodyEl.innerHTML =
        items.length === 0
          ? `<span class="setting-hint">${esc(t("pluginSettingListEmpty"))}</span>`
          : items
              .map(
                (v, i) => `<div class="kv-row"><span class="kv-idx">${i + 1}</span><span class="kv-val">${esc(String(v))}</span><span class="list-ops"><button class="btn ghost" data-list-up="${i}" type="button" aria-label="${escAttr(t("pluginSettingListMoveUp"))}"${rowOff || i === 0 ? " disabled" : ""}>↑</button><button class="btn ghost" data-list-down="${i}" type="button" aria-label="${escAttr(t("pluginSettingListMoveDown"))}"${rowOff || i === items.length - 1 ? " disabled" : ""}>↓</button><button class="btn ghost danger" data-list-del="${i}" type="button" aria-label="${escAttr(t("pluginSettingListDelete"))}"${rowOff ? " disabled" : ""}>×</button></span></div>`,
              )
              .join("");
      bodyEl.querySelectorAll<HTMLButtonElement>("[data-list-up]").forEach((b) =>
        b.addEventListener("click", () => void call({ op: "move", index: Number(b.dataset.listUp), to: Number(b.dataset.listUp) - 1 })),
      );
      bodyEl.querySelectorAll<HTMLButtonElement>("[data-list-down]").forEach((b) =>
        b.addEventListener("click", () => void call({ op: "move", index: Number(b.dataset.listDown), to: Number(b.dataset.listDown) + 1 })),
      );
      bodyEl.querySelectorAll<HTMLButtonElement>("[data-list-del]").forEach((b) =>
        b.addEventListener("click", () => void call({ op: "delete", index: Number(b.dataset.listDel) })),
      );
    };
    row.querySelector(`button[data-list-add="${cssEscape(pair)}"]`)?.addEventListener("click", async () => {
      const input = row.querySelector<HTMLInputElement>("[data-list-new]");
      const v = input?.value.trim();
      if (!v) return; // empty/whitespace-only: don't send (the plugin rejects it too, but no need to round-trip)
      // Clear only on success, and only the part just submitted: a failed value stays in the box for the user to fix (same as saveDeclaredSetting);
      // if the user has already started typing the next one while waiting, that new input must not be wiped by this success response.
      if ((await call({ op: "add", value: v })) && input && input.value.trim() === v) input.value = "";
    });
    void call({ op: "list" });
  });
}

startThemeListener();

void getCurrentWindow()
  .onFocusChanged(({ payload: focused }) => {
    setWindowFocused(focused);
  })
  .catch(() => undefined);

registerLegacyCleanup(() => {
  stopWorkspaceListener();
});
registerLegacyRenderer(renderLegacyTab);
registerTab({ id: "agents", render: renderAgents });
registerTab({ id: "automation", render: renderAutomation });
registerTab({ id: "bubble", render: renderBubbleSettings });
registerTab({ id: "rules", render: renderRules });
registerTab({ id: "backup", render: renderBackup });
registerTab({ id: "hotkeys", render: renderHotkeys });
registerTab({ id: "logs", render: renderLogs, stop: stopLogsStream });
registerTab({ id: "market", render: renderMarket });
registerTab({ id: "plugins", render: renderPlugins });
registerTab({ id: "metrics", render: renderMetricsTab, stop: stopMetricsStream });
registerTab({ id: "notify", render: renderNotify });
registerTab({ id: "profiles", render: renderProfilesTab });
registerTab({ id: "stats", render: renderStats });
registerTab({ id: "sla", render: renderSla });
registerTab({ id: "rpcTrace", render: renderRpcTrace });
registerTab({ id: "capabilities", render: renderCapabilities });
registerTab({ id: "audit", render: renderAudit, stop: stopAuditStream });

void load()
  .then(loadDbRecoveryNotice)
  .then(() => getVersion().then((v) => { setAppVersion(v); }).catch(() => undefined))
  .then(() => {
    startInstallAskListener();
    render();
    void refreshSessions();
    // Fetch once at startup for the sidebar 'Plugin Settings' section: opening settings already has the entry, without clicking another tab first;
    // catch fallback ensures no failure in the startup chain becomes an unhandled rejection.
    void refreshPluginConfig().catch(() => undefined);
    window.setInterval(() => void refreshSessions(), 2000);
  });

// ─── Phase 47: Alerting webhook configuration ──────────────────────────────────────────

interface WebhookHeader { key: string; value: string }

interface WebhookConfigDto {
  enabled: boolean;
  url: string;
  minIntervalSecs: number;
  sources: {
    metricsExceeded: boolean;
    slaViolated: boolean;
    killSwitchEngaged: boolean;
    pluginCrashed: boolean;
  };
  customHeaders: WebhookHeader[];
  schemaVersion: number;
}

let alertingConfig: WebhookConfigDto | null = null;

function setAlertingMsg(text: string): void {
  const m = document.getElementById("alerting-msg");
  if (m) m.textContent = text;
}

async function refreshAlerting(): Promise<void> {
  const form = document.getElementById("alerting-form");
  if (!form) return;
  try {
    alertingConfig = await invoke<WebhookConfigDto>("get_alerting_config");
  } catch (err) {
    form.innerHTML = `<div class="setting-hint">${esc(String(err))}</div>`;
    return;
  }
  renderAlertingForm();
}

function renderAlertingForm(): void {
  const form = document.getElementById("alerting-form");
  if (!form || !alertingConfig) return;
  const c = alertingConfig;
  document.getElementById("alerting-hero-dot")?.classList.toggle("ok", c.enabled);
  form.innerHTML = `
    <div class="alerting-master">
      <div class="alerting-master-info">
        <span class="setting-label">${esc(t("alertingEnabled"))}</span>
        <span class="setting-hint" id="alerting-enabled-state">${esc(t(c.enabled ? "alertingAggregationEnabledOn" : "alertingAggregationEnabledOff"))}</span>
      </div>
      <button class="toggle-switch${c.enabled ? " active" : ""}" id="alerting-enabled" type="button" aria-pressed="${c.enabled}" aria-label="${esc(t("alertingEnabled"))}"><span class="toggle-slider"></span></button>
    </div>
    <div class="alerting-row">
      <label class="alerting-label">${esc(t("alertingUrl"))}</label>
      <input type="text" class="logs-input alerting-url" id="alerting-url" placeholder="https://hooks.slack.com/..." value="${esc(c.url)}" />
    </div>
    <div class="alerting-row">
      <label class="alerting-label">${esc(t("alertingMinInterval"))}</label>
      <input type="number" class="logs-input alerting-interval" id="alerting-interval" min="0" max="3600" value="${c.minIntervalSecs}" />
    </div>
    <div class="alerting-row vertical">
      <label class="alerting-label">${esc(t("alertingSources"))}</label>
      <div class="alerting-sources">
        <label><input type="checkbox" id="alerting-src-metrics"${c.sources.metricsExceeded ? " checked" : ""}/> ${esc(t("alertingSourceMetrics"))}</label>
        <label><input type="checkbox" id="alerting-src-sla"${c.sources.slaViolated ? " checked" : ""}/> ${esc(t("alertingSourceSla"))}</label>
        <label><input type="checkbox" id="alerting-src-ks"${c.sources.killSwitchEngaged ? " checked" : ""}/> ${esc(t("alertingSourceKillSwitch"))}</label>
        <label><input type="checkbox" id="alerting-src-crash"${c.sources.pluginCrashed ? " checked" : ""}/> ${esc(t("alertingSourceCrash"))}</label>
      </div>
    </div>
    <div class="alerting-row vertical">
      <label class="alerting-label">${esc(t("alertingHeaders"))}</label>
      <div id="alerting-headers"></div>
      <button class="btn ghost" id="alerting-add-header" type="button">${esc(t("alertingAddHeader"))}</button>
    </div>
    <div class="alerting-row">
      <label class="alerting-label">${esc(t("alertingSchemaVersion"))}</label>
      <select class="logs-input alerting-schema-version" id="alerting-schema-version">
        <option value="0"${(c.schemaVersion ?? 0) === 0 ? " selected" : ""}>${esc(t("alertingSchemaLegacy"))}</option>
        <option value="1"${(c.schemaVersion ?? 0) === 1 ? " selected" : ""}>${esc(t("alertingSchemaCanonical"))}</option>
      </select>
      <span class="setting-hint">${esc(t("alertingSchemaVersionHint"))}</span>
    </div>`;
  renderAlertingHeaders(c.customHeaders);
  document.getElementById("alerting-enabled")?.addEventListener("click", () => void onAlertingMasterToggle());
  document.getElementById("alerting-url")?.addEventListener("input", () => clearAlertingUrlInvalid());
  document.getElementById("alerting-add-header")?.addEventListener("click", () => {
    if (!alertingConfig) return;
    alertingConfig.customHeaders.push({ key: "", value: "" });
    renderAlertingHeaders(alertingConfig.customHeaders);
  });
}

function renderAlertingHeaders(headers: WebhookHeader[]): void {
  const box = document.getElementById("alerting-headers");
  if (!box) return;
  if (headers.length === 0) {
    box.innerHTML = `<div class="setting-hint">${esc(t("alertingHeadersHint"))}</div>`;
    return;
  }
  box.innerHTML = headers.map((h, i) =>
    `<div class="alerting-header-row">
       <input type="text" class="logs-input alerting-hkey" data-i="${i}" placeholder="${esc(t("alertingHeaderKey"))}" value="${esc(h.key)}" />
       <input type="text" class="logs-input alerting-hval" data-i="${i}" placeholder="${esc(t("alertingHeaderValue"))}" value="${esc(h.value)}" />
       <button class="btn ghost alerting-hdel" data-i="${i}" type="button">×</button>
     </div>`
  ).join("");
  box.querySelectorAll<HTMLInputElement>(".alerting-hkey").forEach((el) => {
    el.addEventListener("input", () => {
      const i = Number(el.dataset.i);
      if (alertingConfig) alertingConfig.customHeaders[i].key = el.value;
    });
  });
  box.querySelectorAll<HTMLInputElement>(".alerting-hval").forEach((el) => {
    el.addEventListener("input", () => {
      const i = Number(el.dataset.i);
      if (alertingConfig) alertingConfig.customHeaders[i].value = el.value;
    });
  });
  box.querySelectorAll<HTMLButtonElement>(".alerting-hdel").forEach((btn) => {
    btn.addEventListener("click", () => {
      const i = Number(btn.dataset.i);
      if (!alertingConfig) return;
      alertingConfig.customHeaders.splice(i, 1);
      renderAlertingHeaders(alertingConfig.customHeaders);
    });
  });
}

/// Toggle visuals + status text + hero dot: touch only these three, don't repaint the whole form (preserving focus/caret of other inputs).
function paintAlertingMasterSwitch(): void {
  const on = alertingConfig?.enabled ?? false;
  const btn = document.getElementById("alerting-enabled");
  btn?.classList.toggle("active", on);
  btn?.setAttribute("aria-pressed", String(on));
  const state = document.getElementById("alerting-enabled-state");
  if (state) state.textContent = t(on ? "alertingAggregationEnabledOn" : "alertingAggregationEnabledOff");
  document.getElementById("alerting-hero-dot")?.classList.toggle("ok", on);
}

function clearAlertingUrlInvalid(): void {
  const urlEl = document.getElementById("alerting-url") as HTMLInputElement | null;
  if (!urlEl) return;
  urlEl.classList.remove("alerting-url-invalid");
  urlEl.removeAttribute("aria-invalid");
}

/// One read path shared by save / toggle / test: sync all current form values into alertingConfig,
/// ensuring no entry point drops other fields the user is typing. enabled is read from the toggle's active class.
function syncAlertingFormToConfig(): void {
  if (!alertingConfig) return;
  const enabledEl = document.getElementById("alerting-enabled");
  const urlEl = document.getElementById("alerting-url") as HTMLInputElement | null;
  const intEl = document.getElementById("alerting-interval") as HTMLInputElement | null;
  const mEl = document.getElementById("alerting-src-metrics") as HTMLInputElement | null;
  const sEl = document.getElementById("alerting-src-sla") as HTMLInputElement | null;
  const kEl = document.getElementById("alerting-src-ks") as HTMLInputElement | null;
  const cEl = document.getElementById("alerting-src-crash") as HTMLInputElement | null;
  const svEl = document.getElementById("alerting-schema-version") as HTMLSelectElement | null;
  alertingConfig.enabled = enabledEl?.classList.contains("active") ?? false;
  alertingConfig.url = urlEl?.value ?? "";
  alertingConfig.minIntervalSecs = Math.max(0, Math.min(3600, Number(intEl?.value ?? 5)));
  alertingConfig.sources.metricsExceeded = mEl?.checked ?? false;
  alertingConfig.sources.slaViolated = sEl?.checked ?? false;
  alertingConfig.sources.killSwitchEngaged = kEl?.checked ?? false;
  alertingConfig.sources.pluginCrashed = cEl?.checked ?? false;
  alertingConfig.schemaVersion = Number(svEl?.value ?? 0);
}

/// master toggle: click-to-save immediately, the hero dot syncs at once; revert the visuals on failure.
/// With an empty URL the backend semantics don't change (it still saves); the frontend only does 'hint + highlight + focus'.
async function onAlertingMasterToggle(): Promise<void> {
  if (!alertingConfig) return;
  const msg = document.getElementById("alerting-msg");
  syncAlertingFormToConfig();
  alertingConfig.enabled = !alertingConfig.enabled;
  paintAlertingMasterSwitch();
  try {
    await invoke("set_alerting_config", { cfg: alertingConfig });
    if (alertingConfig.enabled && alertingConfig.url.trim() === "") {
      if (msg) msg.textContent = `${t("alertingUrl")}: ${t("pluginValidateRequired")}`;
      const urlEl = document.getElementById("alerting-url") as HTMLInputElement | null;
      urlEl?.classList.add("alerting-url-invalid");
      urlEl?.setAttribute("aria-invalid", "true");
      urlEl?.focus();
    } else {
      clearAlertingUrlInvalid();
      if (msg) msg.textContent = t("alertingSaved");
    }
  } catch (err) {
    alertingConfig.enabled = !alertingConfig.enabled;
    paintAlertingMasterSwitch();
    if (msg) msg.textContent = `${t("alertingSaveFailed")}: ${String(err)}`;
  }
}

async function saveAlertingConfig(): Promise<void> {
  const msg = document.getElementById("alerting-msg");
  if (!alertingConfig) return;
  syncAlertingFormToConfig();
  try {
    await invoke("set_alerting_config", { cfg: alertingConfig });
    document.getElementById("alerting-hero-dot")?.classList.toggle("ok", alertingConfig.enabled);
    if (msg) msg.textContent = t("alertingSaved");
  } catch (err) {
    if (msg) msg.textContent = `${t("alertingSaveFailed")}: ${String(err)}`;
  }
}

async function testAlertingWebhook(): Promise<void> {
  const msg = document.getElementById("alerting-msg");
  if (!alertingConfig) return;
  syncAlertingFormToConfig();
  try {
    await invoke("set_alerting_config", { cfg: alertingConfig });
  } catch (err) {
    if (msg) msg.textContent = `${t("alertingSaveFailed")}: ${String(err)}`;
    return;
  }
  try {
    const status = await invoke<number>("test_alerting_webhook");
    if (msg) msg.textContent = `✓ ${t("alertingTestSuccess")} (${status})`;
  } catch (err) {
    if (msg) msg.textContent = `✗ ${t("alertingTestFailed")}: ${String(err)}`;
  }
}

// ─── Phase 49: multi-endpoint fanout ────────────────────────────────────────────

interface WebhookEndpointDto {
  id: string;
  name: string;
  url: string;
  enabled: boolean;
  headers: WebhookHeader[];
  secret: string;
  sourceFilter: string[];
  createdAt: number;
  schemaVersion: number;
  template?: string;        // Phase 57: optional per-endpoint template
  templateSample?: string;  // Phase 58: optional sample JSON for live preview (persisted)
  severityOverrides: Array<{ source: string; severity: string }>; // Phase 72: per-source severity override
}

// Phase 74 — endpoint override dry-run preview DTO
interface EndpointSeverityOverrideHitDto {
  pattern: string;
  severity: string;
  index: number;
}
interface EndpointPreviewRowDto {
  endpointId: string;
  endpointName: string;
  overrideHit: EndpointSeverityOverrideHitDto | null;
  propagationSeverity: string;
  finalEnvelopeSeverity: string;
}
interface EndpointSeverityPreviewDto {
  source: string;
  endpoints: EndpointPreviewRowDto[];
}

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

// Phase 58: live preview DTOs
interface TemplateDiagnostic {
  severity: "error" | "warning";
  code: string;
  message: string;
  offset: number;
  line: number;
  column: number;
}
interface TemplatePreviewResult {
  body: string;
  contentType: string;
  diagnostics: TemplateDiagnostic[];
}

// Phase 59: template preset DTO — returned by the backend list_alerting_template_presets / get_alerting_template_preset
interface TemplatePreset {
  id: string;
  name: string;
  description: string;
  kind: string;       // 'builtin:<slug>' or 'user:<uuid>'
  template: string;
  sample: string;
  builtin: boolean;
  createdAt: number;
  version: number;    // Phase 60: built-in fixed at 1; auto-bumped on user save
  changelog: string;  // Phase 60: optional user-provided changelog
}

// Phase 60: frontend builtin list (one-to-one with backend builtin_presets(); lists only kind + name for the fork prompt)
const FRONTEND_BUILTIN_PRESETS: { kind: string; name: string }[] = [
  { kind: "builtin:slack",        name: "Slack incoming webhook" },
  { kind: "builtin:discord",      name: "Discord webhook" },
  { kind: "builtin:msteams",      name: "Microsoft Teams webhook" },
  { kind: "builtin:generic_json", name: "Generic JSON envelope" },
  { kind: "builtin:plain_text",   name: "Plain text" },
];

let userTemplatePresets: TemplatePreset[] = []; // Phase 59: user custom preset cache

async function refreshTemplatePresets(): Promise<void> {
  try {
    const all = await invoke<TemplatePreset[]>("list_alerting_template_presets");
    userTemplatePresets = all.filter((p) => !p.builtin);
  } catch (e) {
    console.error("list_alerting_template_presets failed:", e);
    userTemplatePresets = [];
  }
}

const DEFAULT_TEMPLATE_SAMPLE = JSON.stringify(
  { user: "alice", count: 42, items: [1, 2, 3], nested: { ok: true } },
  null,
  2,
);

const ALERTING_SOURCES = [
  { id: "plugin.metrics.exceeded", i18n: "alertingSourceMetrics" },
  { id: "capability.sla.violated", i18n: "alertingSourceSla" },
  { id: "plugin.kill_switch.enabled", i18n: "alertingSourceKillSwitch" },
  { id: "plugin.lifecycle.crashed", i18n: "alertingSourceCrash" },
] as const;

let alertingEndpoints: WebhookEndpointDto[] = [];

async function refreshAlertingEndpoints(): Promise<void> {
  await refreshTemplatePresets(); // Phase 59: refresh the user preset cache first, so the UI can render the Custom group
  const list = document.getElementById("alerting-endpoints-list");
  if (!list) return;
  try {
    alertingEndpoints = await invoke<WebhookEndpointDto[]>("list_alerting_endpoints");
  } catch (err) {
    list.innerHTML = `<div class="setting-hint">${esc(String(err))}</div>`;
    return;
  }
  renderAlertingEndpoints();
}

function renderAlertingEndpoints(): void {
  const list = document.getElementById("alerting-endpoints-list");
  if (!list) return;
  if (alertingEndpoints.length === 0) {
    list.innerHTML = `<div class="setting-hint">${esc(t("alertingEndpointsEmpty"))}</div>`;
    return;
  }
  list.innerHTML = alertingEndpoints.map((ep, i) => renderEndpointCard(ep, i)).join("");
  for (const ep of alertingEndpoints) {
    bindEndpointCard(ep);
  }
}

function renderOverrideRow(epId: string, oi: number, srcName: string, sev: string): string {
  const opts = ["info", "warn", "error", "critical"]
    .map((v) => `<option value="${v}"${v === sev ? " selected" : ""}>${v}</option>`)
    .join("");
  return `<div class="alerting-override-row" data-ep-id="${esc(epId)}" data-oi="${oi}">
     <input type="text" class="logs-input alerting-ep-override-source" data-ep-id="${esc(epId)}" data-oi="${oi}" placeholder="${esc(t("alertingEndpointSeverityOverrideSource"))}" value="${esc(srcName)}" style="min-width:200px" />
     <select class="logs-input alerting-ep-override-severity" data-ep-id="${esc(epId)}" data-oi="${oi}">${opts}</select>
     <button class="btn ghost alerting-ep-override-delete" data-ep-id="${esc(epId)}" data-oi="${oi}" type="button" title="${esc(t("alertingEndpointSeverityOverrideDelete"))}">×</button>
   </div>`;
}

function renderEndpointCard(ep: WebhookEndpointDto, i: number): string {
  const sourceBoxes = ALERTING_SOURCES.map((s) => {
    const checked = ep.sourceFilter.includes(s.id) ? " checked" : "";
    return `<label><input type="checkbox" class="alerting-ep-src" data-ep-id="${esc(ep.id)}" data-src="${esc(s.id)}"${checked}/> ${esc(t(s.i18n))}</label>`;
  }).join("");
  const headersHtml = ep.headers.map((h, hi) =>
    `<div class="alerting-header-row">
       <input type="text" class="logs-input alerting-ep-hkey" data-ep-id="${esc(ep.id)}" data-hi="${hi}" placeholder="${esc(t("alertingHeaderKey"))}" value="${esc(h.key)}" />
       <input type="text" class="logs-input alerting-ep-hval" data-ep-id="${esc(ep.id)}" data-hi="${hi}" placeholder="${esc(t("alertingHeaderValue"))}" value="${esc(h.value)}" />
       <button class="btn ghost alerting-ep-hdel" data-ep-id="${esc(ep.id)}" data-hi="${hi}" type="button">×</button>
     </div>`
  ).join("");
  return `
    <div class="alerting-endpoint-card ${ep.enabled ? "alerting-ep-on" : "alerting-ep-off"}" data-ep-id="${esc(ep.id)}" data-i="${i}">
      <div class="alerting-row">
        <label class="alerting-toggle"><input type="checkbox" class="alerting-ep-enabled" data-ep-id="${esc(ep.id)}"${ep.enabled ? " checked" : ""}/> ${esc(t("alertingEndpointEnabled"))}</label>
      </div>
      <div class="alerting-row">
        <label class="alerting-label">${esc(t("alertingEndpointName"))}</label>
        <input type="text" class="logs-input alerting-ep-name" data-ep-id="${esc(ep.id)}" value="${esc(ep.name)}" />
      </div>
      <div class="alerting-row">
        <label class="alerting-label">${esc(t("alertingUrl"))}</label>
        <input type="text" class="logs-input alerting-ep-url" data-ep-id="${esc(ep.id)}" placeholder="https://hooks.example.com/..." value="${esc(ep.url)}" />
      </div>
      <div class="alerting-row vertical">
        <label class="alerting-label">${esc(t("alertingHeaders"))}</label>
        <div class="alerting-ep-headers" data-ep-id="${esc(ep.id)}">${headersHtml || `<div class="setting-hint">${esc(t("alertingHeadersHint"))}</div>`}</div>
        <button class="btn ghost alerting-ep-add-header" data-ep-id="${esc(ep.id)}" type="button">${esc(t("alertingAddHeader"))}</button>
      </div>
      <div class="alerting-row vertical">
        <label class="alerting-label">${esc(t("alertingEndpointSecret"))}</label>
        <input type="password" class="logs-input alerting-ep-secret" data-ep-id="${esc(ep.id)}" placeholder="${esc(t("alertingEndpointSecretHint"))}" value="${esc(ep.secret)}" />
      </div>
      <div class="alerting-row vertical">
        <label class="alerting-label">${esc(t("alertingEndpointSourceFilter"))}</label>
        <div class="alerting-sources">${sourceBoxes}</div>
      </div>
      <div class="alerting-row vertical">
        <details class="alerting-overrides-section">
          <summary class="alerting-label">${esc(t("alertingEndpointSeverityOverrides"))}</summary>
          <span class="setting-hint">${esc(t("alertingEndpointSeverityOverridesHint"))}</span>
          <div class="alerting-overrides-list" data-ep-id="${esc(ep.id)}">
            ${(ep.severityOverrides ?? []).map((o, oi) => renderOverrideRow(ep.id, oi, o.source, o.severity)).join("")}
          </div>
          <button class="btn ghost alerting-ep-override-add" data-ep-id="${esc(ep.id)}" type="button">${esc(t("alertingEndpointSeverityOverrideAdd"))}</button>
        </details>
      </div>
      <div class="alerting-row">
        <label class="alerting-label">${esc(t("alertingSchemaVersion"))}</label>
        <select class="logs-input alerting-ep-schema-version" data-ep-id="${esc(ep.id)}">
          <option value="0"${(ep.schemaVersion ?? 0) === 0 ? " selected" : ""}>${esc(t("alertingSchemaLegacy"))}</option>
          <option value="1"${(ep.schemaVersion ?? 0) === 1 ? " selected" : ""}>${esc(t("alertingSchemaCanonical"))}</option>
        </select>
      </div>
      <div class="alerting-row vertical">
        <label class="alerting-label">${esc(t("alertingEndpointTemplatePreset"))}</label>
        <div class="alerting-template-preset-bar">
          <select class="logs-input alerting-ep-preset-select" data-ep-id="${esc(ep.id)}">
            <option value="">${esc(t("alertingEndpointTemplatePresetNone"))}</option>
            <optgroup label="${esc(t("alertingEndpointTemplatePresetBuiltins"))}">
              <option value="builtin:slack">${esc(t("alertingEndpointTemplatePresetSlack"))}</option>
              <option value="builtin:discord">${esc(t("alertingEndpointTemplatePresetDiscord"))}</option>
              <option value="builtin:msteams">${esc(t("alertingEndpointTemplatePresetMsTeams"))}</option>
              <option value="builtin:generic_json">${esc(t("alertingEndpointTemplatePresetGenericJson"))}</option>
              <option value="builtin:plain_text">${esc(t("alertingEndpointTemplatePresetPlainText"))}</option>
            </optgroup>
            ${userTemplatePresets.length > 0 ? `
              <optgroup label="${esc(t("alertingEndpointTemplatePresetCustom"))}">
                ${userTemplatePresets.map((p) => {
                  const label = !p.builtin && p.version > 1
                    ? `${esc(p.name)} <span class="preset-version-suffix">v${p.version}</span>`
                    : esc(p.name);
                  return `<option value="${esc(p.kind)}">${label}</option>`;
                }).join("")}
              </optgroup>` : ""}
          </select>
          <button class="btn ghost preset-icon-btn alerting-ep-preset-save" data-ep-id="${esc(ep.id)}" title="${esc(t("alertingEndpointTemplatePresetSaveTitle"))}" type="button">💾</button>
          <button class="btn ghost preset-icon-btn alerting-ep-preset-fork" data-ep-id="${esc(ep.id)}" title="${esc(t("alertingEndpointTemplatePresetForkTitle"))}" type="button">🍴</button>
          <button class="btn ghost preset-icon-btn alerting-ep-preset-delete" data-ep-id="${esc(ep.id)}" title="${esc(t("alertingEndpointTemplatePresetDeleteTitle"))}" type="button">🗑</button>
          <button class="btn ghost preset-icon-btn alerting-ep-preset-export" data-ep-id="${esc(ep.id)}" title="${esc(t("alertingEndpointTemplatePresetExportTitle"))}" type="button">⬇</button>
          <button class="btn ghost preset-icon-btn alerting-ep-preset-import" data-ep-id="${esc(ep.id)}" title="${esc(t("alertingEndpointTemplatePresetImportTitle"))}" type="button">⬆</button>
        </div>
        <span class="setting-hint">${esc(t("alertingEndpointTemplatePresetBarHint"))}</span>
        <label class="alerting-label">
          <input type="checkbox" class="alerting-ep-template-toggle" data-ep-id="${esc(ep.id)}"${ep.template ? " checked" : ""}/>
          ${esc(t("alertingEndpointTemplate"))}
        </label>
        <span class="setting-hint">${esc(t("alertingEndpointTemplateHint"))}</span>
        <textarea class="logs-input alerting-ep-template" data-ep-id="${esc(ep.id)}" rows="6" placeholder="${esc(t("alertingEndpointTemplatePlaceholder"))}"${ep.template ? "" : " disabled"}>${esc(ep.template ?? "")}</textarea>
        <span class="setting-hint">${esc(t("alertingEndpointTemplateContentType"))}</span>
        <label class="alerting-label alerting-template-sample-label">${esc(t("alertingEndpointTemplateSample"))}</label>
        <span class="setting-hint">${esc(t("alertingEndpointTemplateSampleHint"))}</span>
        <textarea class="logs-input alerting-ep-sample" data-ep-id="${esc(ep.id)}" rows="6" placeholder="${esc(t("alertingEndpointTemplateSamplePlaceholder"))}"${ep.template ? "" : " disabled"}>${esc(ep.templateSample ?? DEFAULT_TEMPLATE_SAMPLE)}</textarea>
        <div class="endpoint-template-preview-row">
          <span class="endpoint-content-type-badge ct-text" data-ep-id="${esc(ep.id)}">${esc(t("alertingEndpointTemplatePreviewLabel"))}</span>
          <pre class="endpoint-template-preview" data-ep-id="${esc(ep.id)}"></pre>
        </div>
        <div class="endpoint-template-diag" data-ep-id="${esc(ep.id)}"></div>
      </div>
      <div class="alerting-actions">
        <button class="btn ghost alerting-ep-test" data-ep-id="${esc(ep.id)}" type="button">${esc(t("alertingTest"))}</button>
        <button class="btn ghost alerting-ep-preview" data-ep-id="${esc(ep.id)}" type="button" title="${esc(t("alertingSeverityPreviewButton"))}">🔮</button>
        <button class="btn alerting-ep-save" data-ep-id="${esc(ep.id)}" type="button">${esc(t("alertingSave"))}</button>
        <button class="btn ghost alerting-ep-delete" data-ep-id="${esc(ep.id)}" type="button">${esc(t("alertingEndpointDelete"))}</button>
        <span class="setting-hint alerting-ep-msg" data-ep-id="${esc(ep.id)}"></span>
      </div>
    </div>`;
}

function bindEndpointCard(ep: WebhookEndpointDto): void {
  const root = document.querySelector(`.alerting-endpoint-card[data-ep-id="${CSS.escape(ep.id)}"]`);
  if (!root) return;
  // Add header
  root.querySelectorAll<HTMLButtonElement>(".alerting-ep-add-header").forEach((b) => {
    b.addEventListener("click", () => {
      const ep2 = alertingEndpoints.find((x) => x.id === ep.id);
      if (!ep2) return;
      ep2.headers.push({ key: "", value: "" });
      refreshAlertingEndpoints();
    });
  });
  // Delete header
  root.querySelectorAll<HTMLButtonElement>(".alerting-ep-hdel").forEach((b) => {
    b.addEventListener("click", () => {
      const hi = Number(b.dataset.hi);
      const ep2 = alertingEndpoints.find((x) => x.id === ep.id);
      if (!ep2) return;
      ep2.headers.splice(hi, 1);
      refreshAlertingEndpoints();
    });
  });
  // Phase 72: add severity override row -> push an info default straight into the local DTO, refresh renders
  root.querySelectorAll<HTMLButtonElement>(".alerting-ep-override-add").forEach((b) => {
    b.addEventListener("click", () => {
      const ep2 = alertingEndpoints.find((x) => x.id === ep.id);
      if (!ep2) return;
      if (!ep2.severityOverrides) ep2.severityOverrides = [];
      ep2.severityOverrides.push({ source: "", severity: "info" });
      refreshAlertingEndpoints();
    });
  });
  // Phase 72: delete severity override row
  root.querySelectorAll<HTMLButtonElement>(".alerting-ep-override-delete").forEach((b) => {
    b.addEventListener("click", () => {
      const oi = Number(b.dataset.oi);
      const ep2 = alertingEndpoints.find((x) => x.id === ep.id);
      if (!ep2 || !ep2.severityOverrides) return;
      ep2.severityOverrides.splice(oi, 1);
      refreshAlertingEndpoints();
    });
  });
  // save
  root.querySelectorAll<HTMLButtonElement>(".alerting-ep-save").forEach((b) => {
    b.addEventListener("click", () => void onSaveEndpoint(ep.id));
  });
  // delete
  root.querySelectorAll<HTMLButtonElement>(".alerting-ep-delete").forEach((b) => {
    b.addEventListener("click", () => void onDeleteEndpoint(ep.id));
  });
  // test
  root.querySelectorAll<HTMLButtonElement>(".alerting-ep-test").forEach((b) => {
    b.addEventListener("click", () => void onTestEndpoint(ep.id));
  });
  // Phase 74: single-endpoint preview (auto-presets endpointId into the section)
  root.querySelectorAll<HTMLButtonElement>(".alerting-ep-preview").forEach((b) => {
    b.addEventListener("click", () => {
      const sel = document.querySelector<HTMLSelectElement>("#alerting-preview-endpoint");
      if (sel) sel.value = ep.id;
      const srcEl = document.querySelector<HTMLInputElement>("#alerting-preview-source");
      if (srcEl) srcEl.focus();
    });
  });
  // Phase 59: preset picker — selecting a builtin / user preset fills in template + sample
  root.querySelectorAll<HTMLSelectElement>(".alerting-ep-preset-select").forEach((sel) => {
    sel.addEventListener("change", () => {
      void onApplyPreset(ep.id, sel.value);
      // Restore the placeholder option after use, so selecting the same item next time still fires change
      sel.value = "";
    });
  });
  root.querySelectorAll<HTMLButtonElement>(".alerting-ep-preset-save").forEach((b) => {
    b.addEventListener("click", () => void onSavePreset(ep.id));
  });
  // Phase 60: fork builtin -> prompt listing 5 builtins for the user to choose + a new name
  root.querySelectorAll<HTMLButtonElement>(".alerting-ep-preset-fork").forEach((b) => {
    b.addEventListener("click", () => void onForkBuiltin());
  });
  root.querySelectorAll<HTMLButtonElement>(".alerting-ep-preset-delete").forEach((b) => {
    b.addEventListener("click", () => void onDeleteUserPreset(ep.id));
  });
  root.querySelectorAll<HTMLButtonElement>(".alerting-ep-preset-export").forEach((b) => {
    b.addEventListener("click", () => void onExportPresets());
  });
  root.querySelectorAll<HTMLButtonElement>(".alerting-ep-preset-import").forEach((b) => {
    b.addEventListener("click", () => void onImportPresets());
  });
  // template toggle: enable/disable textareas + trigger the first preview
  root.querySelectorAll<HTMLInputElement>(".alerting-ep-template-toggle").forEach((t) => {
    t.addEventListener("change", () => {
      root.querySelectorAll<HTMLTextAreaElement>(".alerting-ep-template, .alerting-ep-sample").forEach((ta) => {
        ta.disabled = !t.checked;
      });
      void refreshTemplatePreview(ep.id, root);
    });
  });
  // live preview debounce
  const tplEl = root.querySelector<HTMLTextAreaElement>(".alerting-ep-template");
  const sampleEl = root.querySelector<HTMLTextAreaElement>(".alerting-ep-sample");
  if (tplEl && sampleEl) {
    let timer = 0;
    const debounced = () => {
      if (timer) window.clearTimeout(timer);
      timer = window.setTimeout(() => void refreshTemplatePreview(ep.id, root), 250);
    };
    tplEl.addEventListener("input", debounced);
    sampleEl.addEventListener("input", debounced);
    // Run once initially (so the user immediately sees the effect of the default sample)
    void refreshTemplatePreview(ep.id, root);
  }
}

// Phase 59: apply a preset (builtin or user) to the current endpoint — fill in template + sample + trigger preview
async function onApplyPreset(epId: string, kind: string): Promise<void> {
  if (!kind) return;
  const root = document.querySelector(`.alerting-endpoint-card[data-ep-id="${CSS.escape(epId)}"]`);
  if (!root) return;
  let p: TemplatePreset | null = null;
  try {
    p = await invoke<TemplatePreset | null>("get_alerting_template_preset", { kind });
  } catch (e) {
    console.error("get_alerting_template_preset failed:", e);
    return;
  }
  if (!p) return;
  // Check the template toggle (if unchecked) and enable the textareas
  const tplToggleEl = root.querySelector<HTMLInputElement>(".alerting-ep-template-toggle");
  const tplEl = root.querySelector<HTMLTextAreaElement>(".alerting-ep-template");
  const sampleEl = root.querySelector<HTMLTextAreaElement>(".alerting-ep-sample");
  if (!tplEl || !sampleEl) return;
  if (tplToggleEl && !tplToggleEl.checked) {
    tplToggleEl.checked = true;
    tplToggleEl.dispatchEvent(new Event("change"));
  }
  tplEl.value = p.template;
  sampleEl.value = p.sample;
  void refreshTemplatePreview(epId, root);
}

// Phase 59: save the current endpoint's template + sample as a user preset. Prompt for name + description.
async function onSavePreset(epId: string): Promise<void> {
  const root = document.querySelector(`.alerting-endpoint-card[data-ep-id="${CSS.escape(epId)}"]`);
  if (!root) return;
  const tplEl = root.querySelector<HTMLTextAreaElement>(".alerting-ep-template");
  const sampleEl = root.querySelector<HTMLTextAreaElement>(".alerting-ep-sample");
  if (!tplEl) return;
  if (!tplEl.value.trim()) {
    setAlertingMsg(t("alertingEndpointTemplatePresetSaveEmpty"));
    return;
  }
  const name = window.prompt(t("alertingEndpointTemplatePresetSavePrompt"));
  if (!name?.trim()) return;
  const description = window.prompt(t("alertingEndpointTemplatePresetSaveDescPrompt")) ?? "";
  try {
    const saved = await invoke<TemplatePreset>("save_alerting_template_preset", {
      preset: {
        id: "",
        name: name.trim(),
        description: description.trim(),
        kind: "",
        template: tplEl.value,
        sample: sampleEl?.value ?? DEFAULT_TEMPLATE_SAMPLE,
        builtin: false,
        createdAt: 0,
      },
    });
    await refreshTemplatePresets();
    setAlertingMsg(`✓ ${t("alertingEndpointTemplatePresetSaved")}: ${saved.name}`);
    // Re-render the endpoint cards; the preset dropdown will bring in the new option
    await refreshAlertingEndpoints();
  } catch (e) {
    setAlertingMsg(`✗ ${e}`);
  }
}

// Phase 59: delete a user preset — a simple select lists existing user presets; pick one, confirm, then call delete
async function onDeleteUserPreset(_epId: string): Promise<void> {
  await refreshTemplatePresets();
  if (userTemplatePresets.length === 0) {
    setAlertingMsg(t("alertingEndpointTemplatePresetDeleteNone"));
    return;
  }
  const list = userTemplatePresets.map((p, i) => `${i + 1}. ${p.name}`).join("\n");
  const ans = window.prompt(`${t("alertingEndpointTemplatePresetDeletePrompt")}\n${list}`);
  if (!ans) return;
  const idx = Number(ans.trim()) - 1;
  if (!Number.isFinite(idx) || idx < 0 || idx >= userTemplatePresets.length) {
    setAlertingMsg(t("alertingEndpointTemplatePresetDeleteInvalid"));
    return;
  }
  const target = userTemplatePresets[idx];
  try {
    await invoke<boolean>("delete_alerting_template_preset", { id: target.id });
    await refreshTemplatePresets();
    setAlertingMsg(`✓ ${t("alertingEndpointTemplatePresetDeleted")}: ${target.name}`);
    await refreshAlertingEndpoints();
  } catch (e) {
    setAlertingMsg(`✗ ${e}`);
  }
}

// Phase 60: fork builtin preset — prompt the user to pick a builtin kind + give a new name -> the backend copies it into a user preset
async function onForkBuiltin(): Promise<void> {
  const list = FRONTEND_BUILTIN_PRESETS
    .map((b, i) => `${i + 1}. ${b.name}`)
    .join("\n");
  const raw = (window.prompt(`${t("alertingEndpointTemplatePresetForkPrompt")}\n${list}`) ?? "").trim();
  if (!raw) return;
  // Accepts: an index '1'-'5' or the kind directly, e.g. 'builtin:slack'
  let resolvedKind = "";
  const num = Number(raw);
  if (Number.isInteger(num) && num >= 1 && num <= FRONTEND_BUILTIN_PRESETS.length) {
    resolvedKind = FRONTEND_BUILTIN_PRESETS[num - 1].kind;
  } else if (raw.startsWith("builtin:")) {
    resolvedKind = raw;
  }
  if (!resolvedKind) {
    setAlertingMsg(t("alertingEndpointTemplatePresetForkInvalid"));
    return;
  }
  const name = window.prompt(t("alertingEndpointTemplatePresetForkNamePrompt"));
  if (!name?.trim()) return;
  try {
    const forked = await invoke<TemplatePreset>("fork_alerting_template_preset", {
      kind: resolvedKind,
      name: name.trim(),
    });
    await refreshTemplatePresets();
    setAlertingMsg(`✓ ${t("alertingEndpointTemplatePresetForked")}: ${forked.name}`);
    await refreshAlertingEndpoints();
  } catch (e) {
    setAlertingMsg(`✗ ${e}`);
  }
}

// Phase 59: export all template presets (builtin + user) to a YAML / JSON file
async function onExportPresets(): Promise<void> {
  try {
    const all = await invoke<TemplatePreset[]>("list_alerting_template_presets");
    const yaml = await invoke<string>("export_alerting_presets", { presets: all });
    const { save } = await import("@tauri-apps/plugin-dialog");
    const path = await save({
      defaultPath: "opencapx-presets.yaml",
      filters: [
        { name: "YAML/JSON", extensions: ["yaml", "yml", "json"] },
      ],
    });
    if (!path) return;
    await invoke("write_text_file", { path, content: yaml });
    setAlertingMsg(`✓ ${t("alertingEndpointTemplatePresetExported")}: ${path}`);
  } catch (e) {
    setAlertingMsg(`✗ ${e}`);
  }
}

// Phase 59: import presets from a YAML / JSON file
async function onImportPresets(): Promise<void> {
  try {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const path = await open({
      multiple: false,
      filters: [
        { name: "YAML/JSON", extensions: ["yaml", "yml", "json"] },
      ],
    });
    if (!path || Array.isArray(path)) return;
    const content = await invoke<string>("read_text_file", { path });
    const count = await invoke<number>("import_alerting_presets", { yaml: content });
    await refreshTemplatePresets();
    // Phase 61: the backend runs the migration chain automatically; logs go to stderr (visible in the tauri console)
    // Here only the success count is shown; migration details go through console.info for devs
    console.info(`[alerting::import] imported ${count} presets; migration log on stderr`);
    setAlertingMsg(`✓ ${t("alertingEndpointTemplatePresetImported")}: ${count}`);
    await refreshAlertingEndpoints();
  } catch (e) {
    setAlertingMsg(`✗ ${e}`);
  }
}

// Phase 62: export the entire alerting config (endpoints + routes + presets + silence + ack) to one YAML file.
// Phase 64: passphrase optional — prompt; empty = signed plaintext, non-empty = AES-GCM encrypted envelope.
async function onExportAlertingBundle(): Promise<void> {
  const msgEl = document.getElementById("alerting-bundle-msg");
  if (msgEl) msgEl.textContent = t("alertingBundleExporting");
  try {
    const pp = (window.prompt(t("alertingBundlePassphrasePrompt")) ?? "").trim();
    if (pp === "_cancel_") return; // sentinel: user closed the prompt directly
    const passphrase = pp === "" ? null : pp;
    const out = await invoke<string>("export_alerting_bundle", { passphrase });
    const encrypted = passphrase !== null;
    const { save } = await import("@tauri-apps/plugin-dialog");
    const path = await save({
      defaultPath: encrypted ? "opencapx-alerting-bundle.enc.json" : "opencapx-alerting-bundle.yaml",
      filters: encrypted
        ? [{ name: "Encrypted JSON", extensions: ["json"] }]
        : [{ name: "YAML", extensions: ["yaml", "yml"] }],
    });
    if (!path) {
      if (msgEl) msgEl.textContent = "";
      return;
    }
    await invoke("write_text_file", { path, content: out });
    const mode = encrypted ? t("alertingBundleEncrypted") : t("alertingBundleSigned");
    if (msgEl) msgEl.textContent = `${t("alertingBundleExported")} (${mode})`;
    setAlertingMsg(`✓ ${t("alertingBundleExported")} (${mode}): ${path}`);
  } catch (e) {
    if (msgEl) msgEl.textContent = String(e);
    setAlertingMsg(`✗ ${e}`);
  }
}

// Phase 62: import the entire alerting config from a YAML file; upsert section by section, forcing preset builtin = false.
// Phase 64: auto-detect plaintext signed vs encrypted envelope; if the file is a JSON envelope, prompt for a passphrase.
async function onImportAlertingBundle(): Promise<void> {
  const msgEl = document.getElementById("alerting-bundle-msg");
  if (msgEl) msgEl.textContent = t("alertingBundleImporting");
  try {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const path = await open({
      multiple: false,
      filters: [
        { name: "Bundle (YAML or encrypted JSON)", extensions: ["yaml", "yml", "json"] },
      ],
    });
    if (!path || Array.isArray(path)) {
      if (msgEl) msgEl.textContent = "";
      return;
    }
    const content = await invoke<string>("read_text_file", { path });
    // Auto-detect: after trim, starting with `{` -> encrypted envelope -> passphrase required
    const isEncrypted = content.trimStart().startsWith("{");
    let passphrase: string | null = null;
    if (isEncrypted) {
      const pp = (window.prompt(t("alertingBundleEncryptedRequiresPassphrase")) ?? "").trim();
      if (pp === "") {
        if (msgEl) msgEl.textContent = t("alertingBundleCancelled");
        return;
      }
      passphrase = pp;
    }
    const summary = await invoke<BundleImportSummary>("import_alerting_bundle", {
      content,
      passphrase,
    });
    if (msgEl) msgEl.textContent = `${t("alertingBundleImported")}: ${summary.total}`;
    setAlertingMsg(`✓ ${t("alertingBundleImported")}: ${summary.total}\n  endpoints: ${summary.endpoints}\n  routes: ${summary.routes}\n  presets: ${summary.presets}\n  silences: ${summary.silences}\n  acks: ${summary.acks}`);
    // Refresh all related views
    await refreshTemplatePresets();
    await refreshAlertingEndpoints();
    await refreshAlertingRoutes();
    await refreshAlertingSilences();
    await refreshAlertingAcks();
  } catch (e) {
    if (msgEl) msgEl.textContent = String(e);
    setAlertingMsg(`✗ ${e}`);
  }
}

// Phase 65: rotate the bundle signing key (generate a new 32 bytes, write to the OS keychain; bundles exported with the old key fail verification)
async function onRotateBundleSecret(): Promise<void> {
  const msgEl = document.getElementById("alerting-bundle-msg");
  if (!window.confirm(t("alertingBundleRotateConfirm"))) return;
  try {
    await invoke<void>("rotate_alerting_bundle_secret");
    if (msgEl) msgEl.textContent = t("alertingBundleRotated");
    setAlertingMsg(`✓ ${t("alertingBundleRotated")}`);
  } catch (e) {
    if (msgEl) msgEl.textContent = String(e);
    setAlertingMsg(`✗ ${e}`);
  }
}

async function refreshTemplatePreview(epId: string, root: Element): Promise<void> {
  const tplEl = root.querySelector<HTMLTextAreaElement>(".alerting-ep-template");
  const sampleEl = root.querySelector<HTMLTextAreaElement>(".alerting-ep-sample");
  const previewEl = root.querySelector<HTMLPreElement>(".endpoint-template-preview");
  const diagEl = root.querySelector<HTMLDivElement>(".endpoint-template-diag");
  const badgeEl = root.querySelector<HTMLSpanElement>(".endpoint-content-type-badge");
  if (!tplEl || !sampleEl || !previewEl || !diagEl) return;
  if (tplEl.disabled) {
    previewEl.textContent = t("alertingEndpointTemplatePreviewDisabled");
    diagEl.innerHTML = "";
    if (badgeEl) badgeEl.className = "endpoint-content-type-badge ct-text";
    return;
  }
  const tpl = tplEl.value;
  let sample: unknown = {};
  try {
    sample = sampleEl.value.trim() ? JSON.parse(sampleEl.value) : {};
    sampleEl.classList.remove("invalid");
  } catch (e) {
    sampleEl.classList.add("invalid");
    previewEl.textContent = `✗ ${t("alertingEndpointTemplateSampleInvalid")}: ${String(e)}`;
    diagEl.innerHTML = "";
    return;
  }
  try {
    const result = await invoke<TemplatePreviewResult>("preview_alerting_template", {
      template: tpl,
      sample,
    });
    previewEl.textContent = result.body || t("alertingEndpointTemplatePreviewEmpty");
    if (badgeEl) {
      badgeEl.textContent = result.contentType;
      badgeEl.className = `endpoint-content-type-badge ${result.contentType.startsWith("application/json") ? "ct-json" : "ct-text"}`;
    }
    if (result.diagnostics.length === 0) {
      diagEl.innerHTML = `<span class="setting-hint">✓ ${esc(t("alertingEndpointTemplateLintClean"))}</span>`;
    } else {
      diagEl.innerHTML = result.diagnostics.map((d) => `
        <div class="endpoint-template-diag-item diag-${esc(d.severity)}">
          <span class="endpoint-template-diag-loc">[line ${d.line}, col ${d.column}]</span>
          <span class="endpoint-template-diag-code">${esc(d.code)}</span>
          <span class="endpoint-template-diag-msg">${esc(d.message)}</span>
        </div>`).join("");
    }
  } catch (err) {
    previewEl.textContent = `✗ ${String(err)}`;
  }
}

async function onSaveEndpoint(id: string): Promise<void> {
  const root = document.querySelector(`.alerting-endpoint-card[data-ep-id="${CSS.escape(id)}"]`);
  const msg = document.querySelector(`.alerting-ep-msg[data-ep-id="${CSS.escape(id)}"]`);
  const ep = alertingEndpoints.find((x) => x.id === id);
  if (!root || !ep) return;
  // Sync current form values
  const nameEl = root.querySelector<HTMLInputElement>(".alerting-ep-name");
  const urlEl = root.querySelector<HTMLInputElement>(".alerting-ep-url");
  const secretEl = root.querySelector<HTMLInputElement>(".alerting-ep-secret");
  const enabledEl = root.querySelector<HTMLInputElement>(".alerting-ep-enabled");
  const srcEls = root.querySelectorAll<HTMLInputElement>(".alerting-ep-src");
  const hkeyEls = root.querySelectorAll<HTMLInputElement>(".alerting-ep-hkey");
  const hvalEls = root.querySelectorAll<HTMLInputElement>(".alerting-ep-hval");
  const svEl = root.querySelector<HTMLSelectElement>(".alerting-ep-schema-version");
  const tplToggleEl = root.querySelector<HTMLInputElement>(".alerting-ep-template-toggle");
  const tplEl = root.querySelector<HTMLTextAreaElement>(".alerting-ep-template");
  const sampleEl = root.querySelector<HTMLTextAreaElement>(".alerting-ep-sample");
  // Collect headers (in DOM order)
  const headers: WebhookHeader[] = [];
  hkeyEls.forEach((k, hi) => {
    const v = hvalEls[hi]?.value ?? "";
    headers.push({ key: k.value, value: v });
  });
  const sourceFilter: string[] = [];
  srcEls.forEach((el) => { if (el.checked) sourceFilter.push(el.dataset.src ?? ""); });
  // Phase 72: collect severity overrides
  const oSrcEls = root.querySelectorAll<HTMLInputElement>(".alerting-ep-override-source");
  const oSevEls = root.querySelectorAll<HTMLSelectElement>(".alerting-ep-override-severity");
  const severityOverrides: Array<{ source: string; severity: string }> = [];
  oSrcEls.forEach((srcEl, oi) => {
    const srcVal = (srcEl.value ?? "").trim();
    if (!srcVal) return; // skip empty source
    const sevVal = oSevEls[oi]?.value ?? "info";
    severityOverrides.push({ source: srcVal, severity: sevVal });
  });
  const payload: WebhookEndpointDto = {
    id: ep.id,
    name: nameEl?.value ?? "",
    url: urlEl?.value ?? "",
    enabled: enabledEl?.checked ?? false,
    headers,
    secret: secretEl?.value ?? "",
    sourceFilter,
    createdAt: ep.createdAt,
    schemaVersion: Number(svEl?.value ?? 0),
    template: (tplToggleEl?.checked && tplEl?.value.trim()) ? tplEl.value : undefined,
    templateSample: (tplToggleEl?.checked && sampleEl?.value.trim()) ? sampleEl.value : undefined,
    severityOverrides,
  };
  try {
    const saved = await invoke<WebhookEndpointDto>("save_alerting_endpoint", { ep: payload });
    // Update the local cache (the backend generates a new id)
    const idx = alertingEndpoints.findIndex((x) => x.id === ep.id);
    if (idx >= 0) alertingEndpoints[idx] = saved;
    if (msg) msg.textContent = `✓ ${t("alertingSaved")}`;
    refreshAlertingEndpoints();
  } catch (err) {
    if (msg) msg.textContent = `✗ ${String(err)}`;
  }
}

async function onDeleteEndpoint(id: string): Promise<void> {
  if (!window.confirm(t("alertingEndpointDeleteConfirm"))) return;
  const msg = document.querySelector(`.alerting-ep-msg[data-ep-id="${CSS.escape(id)}"]`);
  try {
    const cleared = await invoke<[boolean, number]>("delete_alerting_endpoint", { id });
    alertingEndpoints = alertingEndpoints.filter((x) => x.id !== id);
    refreshAlertingEndpoints();
    // Deleting an endpoint may cascade-clear dead letters; also refresh the failed list
    void refreshAlertingFailed();
    if (msg) msg.textContent = `${t("alertingClearExhaustedDone")} (${cleared[1]})`;
  } catch (err) {
    if (msg) msg.textContent = `✗ ${String(err)}`;
  }
}

async function onTestEndpoint(id: string): Promise<void> {
  const msg = document.querySelector(`.alerting-ep-msg[data-ep-id="${CSS.escape(id)}"]`);
  try {
    const status = await invoke<number>("test_alerting_endpoint", { id });
    if (msg) msg.textContent = `✓ ${t("alertingTestSuccess")} (${status})`;
  } catch (err) {
    if (msg) msg.textContent = `✗ ${t("alertingTestFailed")}: ${String(err)}`;
  }
}

async function onAddEndpoint(): Promise<void> {
  // Let the backend generate an id first; once the frontend has it, enter edit mode immediately
  const name = window.prompt(t("alertingEndpointName")) ?? "";
  if (!name.trim()) return;
  const payload: WebhookEndpointDto = {
    id: "",
    name: name.trim(),
    url: "",
    enabled: true,
    headers: [],
    secret: "",
    sourceFilter: [],
    createdAt: 0,
    schemaVersion: 0,
    template: undefined,
    templateSample: undefined,
    severityOverrides: [],
  };
  try {
    const saved = await invoke<WebhookEndpointDto>("save_alerting_endpoint", { ep: payload });
    alertingEndpoints.push(saved);
    refreshAlertingEndpoints();
  } catch (err) {
    alert(`${t("alertingSaveFailed")}: ${String(err)}`);
  }
}

// Phase 74 — endpoint override dry-run preview
function populateAlertingPreviewEndpointSelect(): void {
  const sel = document.querySelector<HTMLSelectElement>("#alerting-preview-endpoint");
  if (!sel) return;
  const current = sel.value;
  const enabled = alertingEndpoints.filter((e) => e.enabled);
  sel.innerHTML =
    `<option value="">${esc(t("alertingSeverityPreviewAllEndpoints"))}</option>` +
    enabled
      .map(
        (e) =>
          `<option value="${esc(e.id)}"${e.id === current ? " selected" : ""}>${esc(e.name)}</option>`,
      )
      .join("");
}

async function onPreviewPredict(): Promise<void> {
  const srcEl = document.querySelector<HTMLInputElement>("#alerting-preview-source");
  const epSel = document.querySelector<HTMLSelectElement>("#alerting-preview-endpoint");
  const result = document.querySelector<HTMLDivElement>("#alerting-preview-result");
  const msg = document.querySelector<HTMLSpanElement>("#alerting-preview-msg");
  if (!srcEl || !result) return;
  const source = srcEl.value.trim();
  if (!source) {
    if (msg) msg.textContent = `✗ ${t("alertingSeverityPreviewSourceRequired")}`;
    return;
  }
  const endpointId = epSel?.value || null;
  try {
    if (msg) msg.textContent = t("alertingSeverityPreviewRunning");
    const preview = await invoke<EndpointSeverityPreviewDto>(
      "preview_alerting_endpoint_severity",
      { source, endpointId },
    );
    if (msg) msg.textContent = `✓ ${t("alertingSeverityPreviewResult")} (${preview.endpoints.length})`;
    if (preview.endpoints.length === 0) {
      result.innerHTML = `<div class="setting-hint">${esc(t("alertingSeverityPreviewNoEndpoints"))}</div>`;
      return;
    }
    result.innerHTML = preview.endpoints.map((row) => renderPreviewRow(row)).join("");
  } catch (err) {
    if (msg) msg.textContent = `✗ ${String(err)}`;
  }
}

function renderPreviewRow(row: EndpointPreviewRowDto): string {
  const hitHtml = row.overrideHit
    ? `<code>${esc(row.overrideHit.pattern)}</code> → <span class="severity-badge sev-${esc(row.overrideHit.severity)}">${esc(row.overrideHit.severity)}</span> <span class="setting-hint">(${t("alertingSeverityPreviewIndex")} ${row.overrideHit.index})</span>`
    : `<em>${esc(t("alertingSeverityPreviewNoOverride"))}</em>`;
  return `<div class="alerting-preview-row">
    <div class="alerting-preview-ep"><strong>${esc(row.endpointName)}</strong></div>
    <div class="alerting-preview-prop">${esc(t("alertingSeverityPreviewPropagation"))}: <span class="severity-badge sev-${esc(row.propagationSeverity)}">${esc(row.propagationSeverity)}</span></div>
    <div class="alerting-preview-override">${esc(t("alertingSeverityPreviewOverride"))}: ${hitHtml}</div>
    <div class="alerting-preview-final">${esc(t("alertingSeverityPreviewFinal"))}: <span class="severity-badge sev-${esc(row.finalEnvelopeSeverity)}">${esc(row.finalEnvelopeSeverity)}</span></div>
  </div>`;
}

// ─── Phase 75: Alerting full-path dry-run simulator ─────────────────────────────

async function onSimulateDispatch(): Promise<void> {
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

// ─── Phase 48: Retry queue + failed deliveries panel ─────────────────────────

interface RetryConfigDto {
  maxAttempts: number;
  initialBackoffSecs: number;
  maxBackoffSecs: number;
  retentionDays: number;
}

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

// ─── Phase 51: DSL routing rules (when/then) ────────────────────────────────────────
interface RouteRuleDto {
  id: string;
  name: string;
  priority: number;
  enabled: boolean;
  kindPattern: string;
  payloadPath: string | null;
  payloadMatch: string | null;
  targetEndpointIds: string[];
  recipients: string[];
  tags: string[];
  // Phase 68 — temporal correlation condition (fan out only if an event matching the pattern occurred within the last N seconds)
  seenInLast: { pattern: string; windowSecs: number } | null;
  createdAt: number;
}

// Phase 68 — correlation analysis
interface RouteSeenEventDto {
  id: string;
  source: string;
  payloadSummary: string;
  tsSecs: number;
  routesFired: string[];
  correlationsHit: string[];
}

interface CycleReportDto {
  cycle: string[];
  // backend serde rename_all = "snake_case" → "self_loop" / "route_to_route" / "route_to_correlation"
  kind: "self_loop" | "route_to_route" | "route_to_correlation";
}

// ─── Phase 67: Alert Recipient persistent CRUD ──────────────────────────────
interface RecipientDto {
  id: string;
  name: string;
  kind: string;
  config: Record<string, unknown>;
  enabled: boolean;
  createdAt: number;
}

// ─── Phase 66: Alert Recipient (multi-channel fanout: webhook / log:stderr / log:file / email:smtp) ─
interface RecipientKindPreset {
  kind: string;
  labelKey: I18nKey;
  specTemplate: string;
  hintKey: I18nKey;
}

const RECIPIENT_KIND_PRESETS: RecipientKindPreset[] = [
  { kind: "log:stderr", labelKey: "alertingRecipientKindLogStderr", specTemplate: "log:stderr", hintKey: "alertingRecipientKindLogStderrHint" },
  { kind: "log:file", labelKey: "alertingRecipientKindLogFile", specTemplate: "log:file:/tmp/opencapx-alerts.log", hintKey: "alertingRecipientKindLogFileHint" },
  { kind: "email:smtp", labelKey: "alertingRecipientKindEmailSmtp", specTemplate: "email:smtp:smtp.example.com:587:alerts@example.com:oncall@example.com", hintKey: "alertingRecipientKindEmailSmtpHint" },
  { kind: "webhook", labelKey: "alertingRecipientKindWebhookRef", specTemplate: "webhook:{endpoint_id}", hintKey: "alertingRecipientWebhookRefHint" },
];

async function onAddRecipient(): Promise<void> {
  // Phase 67 — go through the persistence flow: pick kind -> open a form collecting fields (name + each kind's config) -> save_alerting_recipient.
  // No longer make the user hand-write the spec string (that was the Phase 66 stopgap).
  const menu = RECIPIENT_KIND_PRESETS
    .map((p, i) => `${i + 1}. ${t(p.labelKey)}`)
    .join("\n");
  const choiceRaw = window.prompt(`${t("alertingRecipientAdd")}\n${menu}`);
  if (!choiceRaw?.trim()) return;
  const num = Number(choiceRaw.trim());
  let presetIdx: number;
  if (Number.isInteger(num) && num >= 1 && num <= RECIPIENT_KIND_PRESETS.length) {
    presetIdx = num - 1;
  } else {
    const matched = RECIPIENT_KIND_PRESETS.findIndex((p) => p.kind === choiceRaw.trim());
    if (matched < 0) {
      setAlertingMsg(`✗ ${t("alertingRecipientUnknownKind")}: ${choiceRaw}`);
      return;
    }
    presetIdx = matched;
  }
  const preset = RECIPIENT_KIND_PRESETS[presetIdx];
  const name = window.prompt(t("alertingRecipientNamePrompt"), `my-${preset.kind.replace(":", "-")}`);
  if (!name?.trim()) return;
  // Collect config fields by kind
  let config: Record<string, unknown> = {};
  try {
    if (preset.kind === "log:file") {
      const path = window.prompt(t("alertingRecipientPathPrompt"), "/tmp/opencapx-alerts.log");
      if (!path?.trim()) return;
      config = { path: path.trim() };
    } else if (preset.kind === "email:smtp") {
      const relay = window.prompt(t("alertingRecipientSmtpRelayPrompt"), "smtp.example.com");
      if (!relay?.trim()) return;
      const portStr = window.prompt(t("alertingRecipientSmtpPortPrompt"), "587");
      const port = Number(portStr || "587");
      const from = window.prompt(t("alertingRecipientSmtpFromPrompt"), "alerts@example.com");
      if (!from?.trim()) return;
      const to = window.prompt(t("alertingRecipientSmtpToPrompt"), "oncall@example.com");
      if (!to?.trim()) return;
      config = { relay: relay.trim(), port, from: from.trim(), to: to.trim() };
    } else if (preset.kind === "webhook") {
      let eps: Array<{ id: string; name: string }> = [];
      try {
        eps = await invoke<Array<{ id: string; name: string }>>("list_alerting_endpoints");
      } catch {}
      if (eps.length === 0) {
        setAlertingMsg(`✗ ${t("alertingRouteNeedEndpoint")}`);
        return;
      }
      const epIds = window.prompt(
        t("alertingRecipientWebhookEndpointPrompt"),
        eps.map((e) => `${e.id}(${e.name})`).join(", "),
      );
      if (!epIds?.trim()) return;
      const idPart = epIds.trim().split("(")[0].trim();
      if (!idPart) return;
      config = { endpoint_id: idPart };
    }
    const rec = await invoke<RecipientDto>("save_alerting_recipient", {
      rec: {
        id: "",
        name: name.trim(),
        kind: preset.kind,
        config,
        enabled: true,
        createdAt: 0,
      },
    });
    // Test once immediately
    await invoke<string>("test_alerting_recipient_by_id", { id: rec.id });
    setAlertingMsg(`✓ ${t("alertingRecipientAdded")}: ${rec.name}`);
    await refreshRecipients();
    await refreshAlertingRoutes();
  } catch (e) {
    setAlertingMsg(`✗ ${e}`);
  }
}

async function refreshRecipients(): Promise<void> {
  const list = document.getElementById("alerting-recipients-list");
  if (!list) return;
  let items: RecipientDto[] = [];
  try {
    items = await invoke<RecipientDto[]>("list_alerting_recipients");
  } catch (err) {
    list.innerHTML = `<div class="setting-hint">✗ ${String(err)}</div>`;
    return;
  }
  if (items.length === 0) {
    list.innerHTML = `<div class="setting-hint">${esc(t("alertingRecipientsEmpty"))}</div>`;
    return;
  }
  list.innerHTML = items
    .map((r) => {
      const cfgStr = JSON.stringify(r.config);
      const status = r.enabled
        ? `<span class="alerting-badge alerting-badge-on">${esc(t("alertingRecipientCardEnabled"))}</span>`
        : `<span class="alerting-badge">${esc(t("alertingRecipientCardDisabled"))}</span>`;
      return `<div class="recipient-card${r.enabled ? " recipient-card-active" : ""}" data-id="${esc(r.id)}">
        <div class="recipient-card-head">
          <b>${esc(r.name)}</b>
          <code class="recipient-kind">${esc(r.kind)}</code>
          ${status}
        </div>
        <div class="recipient-card-config"><span class="setting-hint">config:</span> <code>${esc(cfgStr)}</code></div>
        <div class="recipient-card-foot">
          <button class="btn ghost rec-test" data-id="${esc(r.id)}" type="button">${esc(t("alertingRecipientTest"))}</button>
          <button class="btn ghost rec-toggle" data-id="${esc(r.id)}" data-enabled="${r.enabled ? "1" : "0"}" type="button">${r.enabled ? esc(t("alertingRecipientCardDisable")) : esc(t("alertingRecipientCardEnable"))}</button>
          <button class="btn ghost rec-delete" data-id="${esc(r.id)}" type="button">${esc(t("alertingRecipientCardDelete"))}</button>
        </div>
      </div>`;
    })
    .join("");
  list.querySelectorAll<HTMLButtonElement>(".rec-test").forEach((b) => {
    b.addEventListener("click", () => void onTestRecipient(b.dataset.id || ""));
  });
  list.querySelectorAll<HTMLButtonElement>(".rec-toggle").forEach((b) => {
    b.addEventListener("click", () => void onToggleRecipient(b.dataset.id || "", b.dataset.enabled === "1"));
  });
  list.querySelectorAll<HTMLButtonElement>(".rec-delete").forEach((b) => {
    b.addEventListener("click", () => void onDeleteRecipient(b.dataset.id || ""));
  });
}

async function onTestRecipient(id: string): Promise<void> {
  if (!id) return;
  try {
    const result = await invoke<string>("test_alerting_recipient_by_id", { id });
    setAlertingMsg(`✓ ${t("alertingRecipientTested")}\n${result}`);
  } catch (e) {
    setAlertingMsg(`✗ ${e}`);
  }
}

async function onToggleRecipient(id: string, currentEnabled: boolean): Promise<void> {
  if (!id) return;
  try {
    const items = await invoke<RecipientDto[]>("list_alerting_recipients");
    const r = items.find((x) => x.id === id);
    if (!r) return;
    await invoke<RecipientDto>("save_alerting_recipient", {
      rec: { ...r, enabled: !currentEnabled },
    });
    await refreshRecipients();
  } catch (e) {
    setAlertingMsg(`✗ ${e}`);
  }
}

async function onDeleteRecipient(id: string): Promise<void> {
  if (!id) return;
  if (!window.confirm(t("alertingRecipientCardDeleteConfirm"))) return;
  try {
    const [deleted, routesCleared] = await invoke<[boolean, number]>(
      "delete_alerting_recipient",
      { id },
    );
    if (deleted) {
      const suffix = routesCleared > 0
        ? ` (${routesCleared} route ref cleared)`
        : "";
      setAlertingMsg(`✓ ${t("alertingRecipientDeleted")}${suffix}`);
    }
    await refreshRecipients();
    await refreshAlertingRoutes();
  } catch (e) {
    setAlertingMsg(`✗ ${e}`);
  }
}

// Phase 62: per-section counts returned by bundle import.
interface BundleImportSummary {
  endpoints: number;
  routes: number;
  presets: number;
  silences: number;
  acks: number;
  total: number;
}

interface FailedDeliveryDto {
  id: string;
  source: string;
  url: string;
  payload: string;
  firstAttemptTs: number;
  lastAttemptTs: number;
  attempts: number;
  maxAttempts: number;
  lastError: string;
  nextRetryTs: number;
  state: string;
  endpointId?: string;
}

let retryConfig: RetryConfigDto | null = null;
let failedState: string = "";

async function refreshAlertingRetryConfig(): Promise<void> {
  const form = document.getElementById("alerting-retry-form");
  if (!form) return;
  try {
    retryConfig = await invoke<RetryConfigDto>("get_alerting_retry_config");
  } catch (err) {
    form.innerHTML = `<div class="setting-hint">${esc(String(err))}</div>`;
    return;
  }
  const c = retryConfig;
  form.innerHTML = `
    <div class="alerting-row">
      <label class="alerting-label">${esc(t("alertingRetryMaxAttempts"))}</label>
      <input type="number" class="logs-input alerting-interval" id="retry-max-attempts" min="1" max="100" value="${c.maxAttempts}" />
    </div>
    <div class="alerting-row">
      <label class="alerting-label">${esc(t("alertingRetryInitialBackoff"))}</label>
      <input type="number" class="logs-input alerting-interval" id="retry-initial" min="1" max="86400" value="${c.initialBackoffSecs}" />
    </div>
    <div class="alerting-row">
      <label class="alerting-label">${esc(t("alertingRetryMaxBackoff"))}</label>
      <input type="number" class="logs-input alerting-interval" id="retry-max" min="1" max="604800" value="${c.maxBackoffSecs}" />
    </div>
    <div class="alerting-row">
      <label class="alerting-label">${esc(t("alertingRetryRetention"))}</label>
      <input type="number" class="logs-input alerting-interval" id="retry-retention" min="1" max="365" value="${c.retentionDays}" />
    </div>`;
}

async function saveAlertingRetryConfig(): Promise<void> {
  const msg = document.getElementById("alerting-retry-msg");
  const maxA = Number((document.getElementById("retry-max-attempts") as HTMLInputElement | null)?.value ?? 5);
  const init = Number((document.getElementById("retry-initial") as HTMLInputElement | null)?.value ?? 30);
  const maxB = Number((document.getElementById("retry-max") as HTMLInputElement | null)?.value ?? 600);
  const ret = Number((document.getElementById("retry-retention") as HTMLInputElement | null)?.value ?? 7);
  try {
    await invoke("set_alerting_retry_config", {
      cfg: {
        maxAttempts: Math.max(1, Math.min(100, maxA)),
        initialBackoffSecs: Math.max(1, Math.min(86400, init)),
        maxBackoffSecs: Math.max(1, Math.min(604800, maxB)),
        retentionDays: Math.max(1, Math.min(365, ret)),
      },
    });
    if (msg) msg.textContent = t("alertingSaved");
  } catch (err) {
    if (msg) msg.textContent = `${t("alertingSaveFailed")}: ${String(err)}`;
  }
}

async function refreshAlertingFailed(): Promise<void> {
  const sel = document.getElementById("alerting-failed-state") as HTMLSelectElement | null;
  failedState = sel?.value ?? "";
  const list = document.getElementById("alerting-failed-list");
  if (!list) return;
  try {
    const rows = await invoke<FailedDeliveryDto[]>("list_alerting_failed", {
      state: failedState || null,
      limit: 200,
    });
    renderFailedDeliveries(rows);
  } catch (err) {
    list.innerHTML = `<div class="setting-hint">${esc(String(err))}</div>`;
  }
}

function renderFailedDeliveries(rows: FailedDeliveryDto[]): void {
  const list = document.getElementById("alerting-failed-list");
  if (!list) return;
  if (rows.length === 0) {
    list.innerHTML = `<div class="setting-hint">${esc(t("alertingFailedEmpty"))}</div>`;
    return;
  }
  list.innerHTML = `<div class="alerting-failed-table">${rows.map(renderFailedRow).join("")}</div>`;
  list.querySelectorAll<HTMLButtonElement>(".alerting-failed-retry").forEach((btn) => {
    btn.addEventListener("click", () => void onRetryFailed(btn.dataset.id ?? ""));
  });
  list.querySelectorAll<HTMLButtonElement>(".alerting-failed-del").forEach((btn) => {
    btn.addEventListener("click", () => void onDeleteFailed(btn.dataset.id ?? ""));
  });
}

function renderFailedRow(r: FailedDeliveryDto): string {
  const nextRetry = r.nextRetryTs > 0 ? new Date(r.nextRetryTs * 1000).toLocaleString() : "—";
  const stateClass = r.state === "exhausted" ? "alerting-failed-exhausted" : r.state === "resolved" ? "alerting-failed-resolved" : "alerting-failed-pending";
  const stateLabel = r.state === "exhausted" ? t("alertingFailedStateExhausted") : r.state === "resolved" ? t("alertingFailedStateResolved") : t("alertingFailedStatePending");
  // Phase 49 — translate endpointId into an endpoint name (when found in the local cache)
  const epChip = r.endpointId
    ? `<span class="alerting-failed-ep" title="${esc(r.endpointId)}">${esc(alertingEndpoints.find((e) => e.id === r.endpointId)?.name ?? r.endpointId)}</span>`
    : "";
  return `<div class="alerting-failed-row ${stateClass}">
    <div class="alerting-failed-meta"><code>${esc(r.id)}</code> ${epChip} <span class="alerting-failed-source">${esc(r.source)}</span> <span class="alerting-failed-state-badge">${esc(stateLabel)}</span></div>
    <div class="alerting-failed-detail">${esc(t("alertingFailedAttempts"))}: ${r.attempts}/${r.maxAttempts} · ${esc(t("alertingFailedError"))}: ${esc(r.lastError)}</div>
    <div class="alerting-failed-next">${esc(t("alertingFailedNextRetry"))}: ${esc(nextRetry)}</div>
    <div class="alerting-failed-actions">
      <button class="btn ghost alerting-failed-retry" data-id="${esc(r.id)}" type="button">${esc(t("alertingFailedRetry"))}</button>
      <button class="btn ghost alerting-failed-del" data-id="${esc(r.id)}" type="button">${esc(t("alertingFailedDelete"))}</button>
    </div>
  </div>`;
}

async function onRetryFailed(id: string): Promise<void> {
  if (!id) return;
  const msg = document.getElementById("alerting-failed-msg");
  try {
    await invoke("retry_alerting_failed", { id });
    if (msg) msg.textContent = `${t("alertingFailedRetryDone")}: ${id}`;
    void refreshAlertingFailed();
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

async function onDeleteFailed(id: string): Promise<void> {
  if (!id) return;
  const msg = document.getElementById("alerting-failed-msg");
  try {
    await invoke("delete_alerting_failed", { id });
    if (msg) msg.textContent = `${t("alertingFailedDeleteDone")}: ${id}`;
    void refreshAlertingFailed();
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

async function clearAlertingResolved(): Promise<void> {
  const msg = document.getElementById("alerting-failed-msg");
  if (!window.confirm(t("alertingClearExhaustedConfirm"))) return;
  try {
    const n = await invoke<number>("clear_alerting_resolved");
    if (msg) msg.textContent = `${t("alertingClearExhaustedDone")}: ${n}`;
    void refreshAlertingFailed();
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

// ─── Phase 50: silences + acks helpers ────────────────────────────────────────

function weekdayLabel(bit: number): string {
  const names = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
  return names[bit] || "?";
}

function weekdayBitsToLabels(weekdays: number): string {
  const labels: string[] = [];
  for (let i = 0; i < 7; i++) {
    if (weekdays & (1 << i)) labels.push(weekdayLabel(i));
  }
  return labels.length === 7 ? t("alertingEveryday") : labels.join(", ");
}

function formatUnix(ts: number): string {
  const d = new Date(ts * 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())} ${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())} UTC`;
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

async function refreshAlertingSilences(): Promise<void> {
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
          <div class="silence-card-foot"><button class="btn ghost" data-silence-del="${esc(s.id)}" type="button">${esc(t("alertingSilenceDelete"))}</button></div>
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

async function onAddSilence(): Promise<void> {
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

async function refreshAlertingAcks(): Promise<void> {
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
          <div class="ack-card-foot"><button class="btn ghost" data-ack-del="${esc(a.id)}" type="button">${esc(t("alertingSilenceDelete"))}</button></div>
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

async function onAckKind(): Promise<void> {
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

// ─── Phase 51: DSL routes helpers ──────────────────────────────────────────────

async function refreshAlertingRoutes(): Promise<void> {
  const list = document.getElementById("alerting-routes-list");
  const msg = document.getElementById("alerting-route-msg");
  if (!list) return;
  try {
    const items = await invoke<RouteRuleDto[]>("list_alerting_routes");
    if (items.length === 0) {
      list.innerHTML = `<span class="setting-hint">${esc(t("alertingRoutesEmpty"))}</span>`;
      return;
    }
    // Build endpoint name lookup for richer target rendering
    const eps = await invoke<WebhookEndpointDto[]>("list_alerting_endpoints");
    const nameById = new Map<string, string>(eps.map((e) => [e.id, e.name] as [string, string]));
    list.innerHTML = items
      .map((r) => {
        const status = r.enabled
          ? `<span class="alerting-badge alerting-badge-on">${esc(t("alertingRouteEnabled"))}</span>`
          : `<span class="alerting-badge">${esc(t("alertingRouteDisabled"))}</span>`;
        const targetNames = r.targetEndpointIds
          .map((id) => nameById.get(id) || id)
          .join(", ");
        const recipientsLine = (r.recipients ?? []).length > 0
          ? `<div class="route-card-row"><span class="setting-hint">${esc(t("alertingRouteRecipients"))}:</span> <code>${esc(r.recipients.join(", "))}</code></div>`
          : "";
        const whenClause = r.payloadPath && r.payloadMatch
          ? `${esc(r.kindPattern)}<br/><span class="setting-hint">→ ${esc(r.payloadPath)} <code>${esc(r.payloadMatch)}</code></span>`
          : `${esc(r.kindPattern)}`;
        const tags = r.tags.length > 0
          ? `<div class="route-tags">${r.tags.map((tg) => `<span class="route-tag">${esc(tg)}</span>`).join("")}</div>`
          : "";
        return `<div class="route-card${r.enabled ? " route-card-active" : ""}">
          <div class="route-card-head">
            <b>${esc(r.name)}</b>
            <span class="route-priority">#${r.priority}</span>
            ${status}
          </div>
          <div class="route-card-row"><span class="setting-hint">${esc(t("alertingRouteWhen"))}:</span> ${whenClause}</div>
          <div class="route-card-row"><span class="setting-hint">${esc(t("alertingRouteThen"))}:</span> <code>${esc(targetNames)}</code></div>
          ${recipientsLine}
          ${tags}
          <div class="route-card-foot"><button class="btn ghost" data-route-del="${esc(r.id)}" type="button">${esc(t("alertingSilenceDelete"))}</button><button class="btn ghost" data-route-toggle="${esc(r.id)}" type="button">${r.enabled ? esc(t("alertingRouteDisable")) : esc(t("alertingRouteEnable"))}</button></div>
        </div>`;
      })
      .join("");
    list.querySelectorAll<HTMLButtonElement>("[data-route-del]").forEach((btn) => {
      btn.addEventListener("click", () => void deleteRoute(btn.dataset.routeDel || ""));
    });
    list.querySelectorAll<HTMLButtonElement>("[data-route-toggle]").forEach((btn) => {
      btn.addEventListener("click", () => void toggleRoute(btn.dataset.routeToggle || ""));
    });
    if (msg) msg.textContent = "";
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

async function deleteRoute(id: string): Promise<void> {
  if (!id) return;
  if (!window.confirm(t("alertingRouteDeleteConfirm"))) return;
  const msg = document.getElementById("alerting-route-msg");
  try {
    await invoke<boolean>("delete_alerting_route", { id });
    if (msg) msg.textContent = t("alertingRouteDeleted");
    void refreshAlertingRoutes();
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

async function toggleRoute(id: string): Promise<void> {
  if (!id) return;
  const msg = document.getElementById("alerting-route-msg");
  try {
    const items = await invoke<RouteRuleDto[]>("list_alerting_routes");
    const r = items.find((x) => x.id === id);
    if (!r) return;
    await invoke("save_alerting_route", {
      rule: {
        id: r.id,
        name: r.name,
        priority: r.priority,
        enabled: !r.enabled,
        kindPattern: r.kindPattern,
        payloadPath: r.payloadPath,
        payloadMatch: r.payloadMatch,
        targetEndpointIds: r.targetEndpointIds,
        recipients: r.recipients ?? [],
        tags: r.tags,
      },
    });
    void refreshAlertingRoutes();
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

async function onAddRoute(): Promise<void> {
  const msg = document.getElementById("alerting-route-msg");
  const name = window.prompt(t("alertingRouteNamePrompt"), "critical -> oncall");
  if (!name) return;
  const pattern = window.prompt(t("alertingRoutePatternPrompt"), "*") || "*";
  const priorityStr = window.prompt(t("alertingRoutePriorityPrompt"), "100") || "100";
  const priority = parseInt(priorityStr, 10);
  if (isNaN(priority)) {
    if (msg) msg.textContent = t("alertingRoutePriorityInvalid");
    return;
  }
  // pick endpoint ids
  let eps: WebhookEndpointDto[] = [];
  try {
    eps = await invoke<WebhookEndpointDto[]>("list_alerting_endpoints");
  } catch {}
  if (eps.length === 0) {
    if (msg) msg.textContent = t("alertingRouteNeedEndpoint");
    return;
  }
  const idsStr = window.prompt(
    t("alertingRouteTargetPrompt"),
    eps.map((e) => `${e.id}(${e.name})`).join(", "),
  );
  if (!idsStr) return;
  const targetIds: string[] = [];
  for (let token of idsStr.split(",")) {
    token = token.trim();
    if (!token) continue;
    // Accepts id or id(name) form
    const idPart = token.split("(")[0].trim();
    if (idPart) targetIds.push(idPart);
  }
  if (targetIds.length === 0) {
    if (msg) msg.textContent = t("alertingRouteTargetRequired");
    return;
  }
  // Phase 68 — temporal correlation condition (optional)
  let seenInLast: { pattern: string; windowSecs: number } | null = null;
  const seenRaw = window.prompt(t("alertingRouteSeenInLastPrompt"), "");
  if (seenRaw?.trim()) {
    const parts = seenRaw.split("|").map((s) => s.trim());
    const seenPat = parts[0] ?? "";
    const seenWin = parseInt(parts[1] ?? "60", 10);
    if (seenPat) {
      seenInLast = { pattern: seenPat, windowSecs: Number.isFinite(seenWin) && seenWin > 0 ? seenWin : 60 };
    }
  }
  try {
    await invoke("save_alerting_route", {
      rule: {
        id: "",
        name,
        priority,
        enabled: true,
        kindPattern: pattern,
        payloadPath: null,
        payloadMatch: null,
        targetEndpointIds: targetIds,
        recipients: [],
        tags: [],
        seenInLast,
      },
    });
    if (msg) msg.textContent = t("alertingRouteCreated");
    void refreshAlertingRoutes();
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

async function onImportRoutesYaml(): Promise<void> {
  const msg = document.getElementById("alerting-route-msg");
  const yaml = window.prompt(t("alertingRouteImportPrompt"), "version: 1\nrules: []\n");
  if (!yaml) return;
  try {
    const n = await invoke<number>("import_alerting_routes_yaml", { yaml });
    if (msg) msg.textContent = `${t("alertingRouteImported")}: ${n}`;
    void refreshAlertingRoutes();
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

async function onExportRoutesYaml(): Promise<void> {
  const msg = document.getElementById("alerting-route-msg");
  try {
    const yaml = await invoke<string>("export_alerting_routes_yaml");
    // Write the YAML to the clipboard so the user can copy it easily
    try {
      await navigator.clipboard.writeText(yaml);
      if (msg) msg.textContent = t("alertingRouteExportedClipboard");
    } catch {
      window.prompt(t("alertingRouteExportPrompt"), yaml);
      if (msg) msg.textContent = t("alertingRouteExported");
    }
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

async function onDryRunRoute(): Promise<void> {
  const msg = document.getElementById("alerting-route-msg");
  const source = window.prompt(t("alertingRouteDryRunSourcePrompt"), "plugin.metrics.exceeded") || "";
  if (!source) return;
  const payloadJson = window.prompt(t("alertingRouteDryRunPayloadPrompt"), '{"cpu_percent": 95}') || "";
  try {
    const hit = await invoke<RouteRuleDto | null>("dry_run_alerting_route", {
      source,
      payloadJson,
    });
    if (hit) {
      if (msg) msg.textContent = `${t("alertingRouteDryRunHit")}: ${hit.name} (#${hit.priority})`;
    } else {
      if (msg) msg.textContent = t("alertingRouteDryRunNoHit");
    }
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

// ─── Phase 53: Alerting severity hints ─────────────────────────────────────────

interface SeverityHintDto {
  source: string;
  severity: string;
  origin: string;
  pluginId?: string;
  updatedAt: number;
  effectiveSeverity: string;   // Phase 69 — effective severity actually applied
}

interface SeverityLinkDto {
  policy: "manifest" | "plugin_default" | "user_override" | "disabled";
  severity: string | null;
  source: string;
  pluginId?: string;
  hit: boolean;
}

interface CascadeReportDto {
  hintDeleted: boolean;
  affectedRoutes: string[];
  affectedCorrelations: string[];
  affectedAggregations: string[];
}

interface PropagationTraceDto {
  source: string;
  routeSeverity: string;
  routeOrigin: "manifest" | "plugin_default" | "user_override" | "disabled";
  correlationSeverity: string;
  correlationOrigin: "manifest" | "plugin_default" | "user_override" | "disabled";
  aggregationSeverity: string;
  aggregationOrigin: "manifest" | "plugin_default" | "user_override" | "disabled";
  escalationSeverity: string;
  escalationOrigin: "manifest" | "plugin_default" | "user_override" | "disabled";
}

async function refreshAlertingSeverityHints(): Promise<void> {
  const list = document.getElementById("alerting-severity-hints-list");
  if (!list) return;
  let hints: SeverityHintDto[] = [];
  try {
    hints = await invoke<SeverityHintDto[]>("list_alerting_severity_hints");
  } catch (err) {
    list.innerHTML = `<div class="setting-hint">${esc(String(err))}</div>`;
    return;
  }
  if (hints.length === 0) {
    list.innerHTML = `<div class="setting-hint">${esc(t("alertingSeverityHintsEmpty"))}</div>`;
    return;
  }
  list.innerHTML = hints.map((h) => {
    const opts = ["info", "warn", "error", "critical"].map((s) =>
      `<option value="${s}"${s === h.severity ? " selected" : ""}>${s}</option>`
    ).join("");
    const originLabel = h.origin === "manifest"
      ? t("alertingSeverityHintOriginManifest")
      : t("alertingSeverityHintOriginUser");
    const owner = h.pluginId ? ` <span class="setting-hint">@${esc(h.pluginId)}</span>` : "";
    const effective = h.effectiveSeverity !== h.severity
      ? ` <span class="severity-hint-effective">${esc(t("alertingSeverityEffective"))} <strong>${esc(h.effectiveSeverity)}</strong></span>`
      : "";
    return `
      <div class="severity-hint-card severity-hint-card-${esc(h.severity)}" data-source="${esc(h.source)}">
        <code class="severity-hint-source">${esc(h.source)}</code>
        <select class="logs-input severity-hint-severity" data-source="${esc(h.source)}">${opts}</select>
        <span class="severity-origin-badge severity-origin-badge-${esc(h.origin)}">${esc(originLabel)}</span>
        ${owner}
        ${effective}
        <button class="btn ghost severity-hint-preview" data-source="${esc(h.source)}" type="button" title="${esc(t("alertingSeverityPreviewChain"))}">↻</button>
        <button class="btn ghost severity-hint-propagation" data-source="${esc(h.source)}" type="button" title="${esc(t("alertingSeverityPreviewPropagation"))}">↗</button>
        <button class="btn ghost severity-hint-cascade" data-source="${esc(h.source)}" type="button" title="${esc(t("alertingSeverityCascadeDelete"))}">⌫</button>
        <button class="btn ghost severity-hint-delete" data-source="${esc(h.source)}" type="button">×</button>
      </div>`;
  }).join("");
  list.querySelectorAll<HTMLSelectElement>(".severity-hint-severity").forEach((el) => {
    el.addEventListener("change", () => {
      const source = el.dataset.source ?? "";
      void onUpdateSeverityHint(source, el.value);
    });
  });
  list.querySelectorAll<HTMLButtonElement>(".severity-hint-delete").forEach((btn) => {
    btn.addEventListener("click", () => {
      const source = btn.dataset.source ?? "";
      void onDeleteSeverityHint(source);
    });
  });
  list.querySelectorAll<HTMLButtonElement>(".severity-hint-preview").forEach((btn) => {
    btn.addEventListener("click", () => {
      const source = btn.dataset.source ?? "";
      void onPreviewSeverityChain(source);
    });
  });
  list.querySelectorAll<HTMLButtonElement>(".severity-hint-propagation").forEach((btn) => {
    btn.addEventListener("click", () => {
      const source = btn.dataset.source ?? "";
      void onPreviewPropagation(source);
    });
  });
  list.querySelectorAll<HTMLButtonElement>(".severity-hint-cascade").forEach((btn) => {
    btn.addEventListener("click", () => {
      const source = btn.dataset.source ?? "";
      void onCascadeDeleteSeverityHint(source);
    });
  });
}

async function onAddSeverityHint(): Promise<void> {
  const msg = document.getElementById("severity-hints-msg");
  const srcEl = document.getElementById("severity-hint-source") as HTMLInputElement | null;
  const sevEl = document.getElementById("severity-hint-severity") as HTMLSelectElement | null;
  const source = (srcEl?.value ?? "").trim();
  const severity = sevEl?.value ?? "warn";
  if (!source) {
    if (msg) msg.textContent = t("alertingSeverityHintAddInvalid");
    return;
  }
  try {
    await invoke("save_alerting_severity_hint", { source, severity });
    if (srcEl) srcEl.value = "";
    if (msg) msg.textContent = `✓ ${t("alertingSaved")}`;
    void refreshAlertingSeverityHints();
    void refreshAlertingAggregations();
    void refreshAlertingCorrelations();
  } catch (err) {
    if (msg) msg.textContent = `✗ ${String(err)}`;
  }
}

async function onUpdateSeverityHint(source: string, severity: string): Promise<void> {
  const msg = document.getElementById("severity-hints-msg");
  try {
    await invoke("save_alerting_severity_hint", { source, severity });
    if (msg) msg.textContent = `✓ ${t("alertingSaved")}`;
  } catch (err) {
    if (msg) msg.textContent = `✗ ${String(err)}`;
  }
}

async function onDeleteSeverityHint(source: string): Promise<void> {
  const msg = document.getElementById("severity-hints-msg");
  if (!window.confirm(t("alertingSeverityHintsClearConfirm") + ` (${source})`)) return;
  try {
    const removed = await invoke<boolean>("delete_alerting_severity_hint", { source });
    if (msg) msg.textContent = removed ? `✓ ${t("alertingSaved")}` : `(manifest-origin; use uninstall plugin)`;
    void refreshAlertingSeverityHints();
    void refreshAlertingAggregations();
    void refreshAlertingCorrelations();
  } catch (err) {
    if (msg) msg.textContent = `✗ ${String(err)}`;
  }
}

async function clearAllSeverityHints(): Promise<void> {
  const msg = document.getElementById("severity-hints-msg");
  if (!window.confirm(t("alertingSeverityHintsClearConfirm"))) return;
  try {
    const n = await invoke<number>("clear_alerting_severity_hints");
    if (msg) msg.textContent = `${t("alertingSaved")} (${n})`;
    void refreshAlertingSeverityHints();
    void refreshAlertingAggregations();
    void refreshAlertingCorrelations();
  } catch (err) {
    if (msg) msg.textContent = `✗ ${String(err)}`;
  }
}

// Phase 69 — severity policy chain preview + cascade delete
function policyLabel(policy: SeverityLinkDto["policy"]): string {
  switch (policy) {
    case "manifest":       return t("alertingSeverityPolicyManifest");
    case "plugin_default": return t("alertingSeverityPolicyPluginDefault");
    case "user_override":  return t("alertingSeverityPolicyUserOverride");
    case "disabled":       return t("alertingSeverityPolicyDisabled");
    default:               return policy;
  }
}

async function onPreviewSeverityChain(source: string): Promise<void> {
  const result = document.getElementById("alerting-severity-chain-result");
  const msg = document.getElementById("severity-hints-msg");
  if (!result) return;
  try {
    const links = await invoke<SeverityLinkDto[]>("severity_inheritance_chain", { source });
    const rows = links.map((l) => {
      const mark = l.hit ? "●" : "○";
      const sevText = l.severity ?? "—";
      const plugin = l.pluginId ? ` <span class="setting-hint">@${esc(l.pluginId)}</span>` : "";
      const cls = l.hit ? "severity-link-row severity-link-hit" : "severity-link-row";
      return `<div class="${cls}"><code>${mark}</code> <strong>${esc(policyLabel(l.policy))}</strong> → <code>${esc(sevText)}</code>${plugin}</div>`;
    }).join("");
    result.innerHTML = `<div class="setting-hint">${esc(source)}</div>${rows}`;
    if (msg) msg.textContent = `✓ ${t("alertingSaved")}`;
  } catch (err) {
    if (msg) msg.textContent = `✗ ${String(err)}`;
  }
}

async function onCascadeDeleteSeverityHint(source: string): Promise<void> {
  const result = document.getElementById("alerting-severity-cascade-result");
  const msg = document.getElementById("severity-hints-msg");
  if (!window.confirm(t("alertingSeverityCascadeConfirm") + ` (${source})`)) return;
  try {
    const report = await invoke<CascadeReportDto>("delete_alerting_severity_hint_cascade", { source });
    const parts: string[] = [];
    if (report.hintDeleted) parts.push(`✓ ${t("alertingSeverityCascadeDone")}`);
    parts.push(`${t("alertingSeverityAffectedRoutes")}: <strong>${report.affectedRoutes.length}</strong>`);
    parts.push(`${t("alertingSeverityAffectedCorrelations")}: <strong>${report.affectedCorrelations.length}</strong>`);
    parts.push(`${t("alertingSeverityAffectedAggregations")}: <strong>${report.affectedAggregations.length}</strong>`);
    if (result) result.innerHTML = `<div class="setting-hint">${esc(source)} → ${parts.join(" · ")}</div>`;
    if (msg) msg.textContent = `✓ ${t("alertingSaved")}`;
    void refreshAlertingSeverityHints();
    void refreshAlertingRoutes();
    void refreshAlertingCorrelations();
    void refreshAlertingAggregations();
  } catch (err) {
    if (msg) msg.textContent = `✗ ${String(err)}`;
  }
}

// Phase 70 — severity cross-chain propagation preview
async function onPreviewPropagation(source: string): Promise<void> {
  const result = document.getElementById("alerting-severity-propagation-result");
  const msg = document.getElementById("severity-hints-msg");
  if (!result) return;
  try {
    const tr = await invoke<PropagationTraceDto>("severity_propagation_trace", { source });
    const rows: [string, string, "manifest" | "plugin_default" | "user_override" | "disabled"][] = [
      [t("alertingSeverityPropagationRoute"),       tr.routeSeverity,       tr.routeOrigin],
      [t("alertingSeverityPropagationCorrelation"), tr.correlationSeverity, tr.correlationOrigin],
      [t("alertingSeverityPropagationAggregation"), tr.aggregationSeverity, tr.aggregationOrigin],
      [t("alertingSeverityPropagationEscalation"),  tr.escalationSeverity,  tr.escalationOrigin],
    ];
    result.innerHTML = `<div class="setting-hint">${esc(source)}</div>` + rows.map(([label, sev, origin]) =>
      `<div class="severity-propagation-row">
         <span class="severity-propagation-label">${esc(label)}</span>
         <code class="severity-propagation-severity">${esc(sev)}</code>
         <span class="severity-origin-badge severity-origin-badge-${esc(origin)}">${esc(policyLabel(origin))}</span>
       </div>`
    ).join("");
    if (msg) msg.textContent = `✓ ${t("alertingSaved")}`;
  } catch (err) {
    if (msg) msg.textContent = `✗ ${String(err)}`;
  }
}

// ─── Phase 54: alerting aggregation / frequency threshold rules ──────────────────────────────────

interface AggregationRuleDto {
  id: string;
  name: string;
  kindPattern: string;
  windowSecs: number;
  thresholdCount: number;
  action: string;
  targetSeverity?: string | null;
  enabled: boolean;
  createdAt: number;
}

function aggMsg(text: string, ok: boolean): void {
  const msg = document.getElementById("agg-msg");
  if (msg) msg.textContent = `${ok ? "✓" : "✗"} ${text}`;
}

function updateAggTargetSeverityVisibility(): void {
  const sel = document.getElementById("agg-action") as HTMLSelectElement | null;
  const tgt = document.getElementById("agg-target-severity") as HTMLSelectElement | null;
  if (!sel || !tgt) return;
  tgt.disabled = sel.value !== "downgrade";
}

async function refreshAlertingAggregations(): Promise<void> {
  const list = document.getElementById("alerting-aggregations-list");
  if (!list) return;
  updateAggTargetSeverityVisibility();
  let rules: AggregationRuleDto[] = [];
  try {
    rules = await invoke<AggregationRuleDto[]>("list_alerting_aggregations");
  } catch (err) {
    list.innerHTML = `<div class="setting-hint">✗ ${String(err)}</div>`;
    return;
  }
  if (rules.length === 0) {
    list.innerHTML = `<div class="setting-hint">${esc(t("alertingAggregationsEmpty"))}</div>`;
    return;
  }
  list.innerHTML = rules
    .map((r) => {
      const tgt = r.action === "downgrade" && r.targetSeverity
        ? `<span class="agg-target-severity">→ ${esc(r.targetSeverity)}</span>`
        : "";
      const enabledBadge = `<span class="agg-enabled-badge ${r.enabled ? "on" : "off"}">${r.enabled ? esc(t("alertingAggregationEnabledOn")) : esc(t("alertingAggregationEnabledOff"))}</span>`;
      const toggle = `<button class="btn ghost agg-toggle" data-id="${esc(r.id)}" type="button">${r.enabled ? esc(t("alertingAggregationDisable")) : esc(t("alertingAggregationEnable"))}</button>`;
      return `<div class="agg-card ${r.enabled ? "agg-card-on" : "agg-card-off"}" data-id="${esc(r.id)}">
        <span class="agg-name">${esc(r.name)}</span>
        <code class="agg-pattern">${esc(r.kindPattern)}</code>
        <span class="agg-count">${r.thresholdCount}× / ${r.windowSecs}s</span>
        <span class="agg-action-badge agg-action-${esc(r.action)}">${esc(r.action)}</span>
        ${tgt}
        ${enabledBadge}
        ${toggle}
        <button class="btn ghost agg-delete" data-id="${esc(r.id)}" type="button">${esc(t("alertingAggregationDelete"))}</button>
      </div>`;
    })
    .join("");
  list.querySelectorAll<HTMLButtonElement>(".agg-delete").forEach((btn) => {
    btn.addEventListener("click", () => void onDeleteAggregation(btn.dataset.id || ""));
  });
  list.querySelectorAll<HTMLButtonElement>(".agg-toggle").forEach((btn) => {
    btn.addEventListener("click", () => void onToggleAggregation(btn.dataset.id || ""));
  });
}

async function onAddAggregation(): Promise<void> {
  const name = (document.getElementById("agg-name") as HTMLInputElement)?.value.trim() || "";
  const pattern = (document.getElementById("agg-pattern") as HTMLInputElement)?.value.trim() || "*";
  const windowSecs = parseInt((document.getElementById("agg-window") as HTMLInputElement)?.value || "0", 10);
  const threshold = parseInt((document.getElementById("agg-threshold") as HTMLInputElement)?.value || "0", 10);
  const action = (document.getElementById("agg-action") as HTMLSelectElement)?.value || "suppress";
  const targetSeverity = (document.getElementById("agg-target-severity") as HTMLSelectElement)?.value || null;
  if (!name) {
    aggMsg(t("alertingAggregationNameRequired"), false);
    return;
  }
  if (windowSecs <= 0 || threshold <= 0) {
    aggMsg(t("alertingAggregationInvalid"), false);
    return;
  }
  try {
    const rule: AggregationRuleDto = {
      id: "",
      name,
      kindPattern: pattern,
      windowSecs,
      thresholdCount: threshold,
      action,
      targetSeverity: action === "downgrade" ? targetSeverity : null,
      enabled: true,
      createdAt: 0,
    };
    const saved = await invoke<AggregationRuleDto>("save_alerting_aggregation", { rule });
    aggMsg(`${t("alertingSaved")}: ${saved.name}`, true);
    const n = document.getElementById("agg-name") as HTMLInputElement | null;
    if (n) n.value = "";
    void refreshAlertingAggregations();
  } catch (err) {
    aggMsg(String(err), false);
  }
}

async function onDeleteAggregation(id: string): Promise<void> {
  if (!id) return;
  if (!window.confirm(t("alertingAggregationDeleteConfirm"))) return;
  try {
    const ok = await invoke<boolean>("delete_alerting_aggregation", { id });
    aggMsg(ok ? t("alertingSaved") : t("alertingAggregationDeleteFailed"), ok);
    void refreshAlertingAggregations();
  } catch (err) {
    aggMsg(String(err), false);
  }
}

async function onToggleAggregation(id: string): Promise<void> {
  if (!id) return;
  try {
    const rules = await invoke<AggregationRuleDto[]>("list_alerting_aggregations");
    const r = rules.find((x) => x.id === id);
    if (!r) {
      aggMsg(t("alertingAggregationNotFound"), false);
      return;
    }
    const updated: AggregationRuleDto = { ...r, enabled: !r.enabled };
    await invoke<AggregationRuleDto>("save_alerting_aggregation", { rule: updated });
    aggMsg(`${t("alertingSaved")}: ${updated.name} (${updated.enabled ? "on" : "off"})`, true);
    void refreshAlertingAggregations();
  } catch (err) {
    aggMsg(String(err), false);
  }
}

async function clearAllAggregations(): Promise<void> {
  if (!window.confirm(t("alertingAggregationClearConfirm"))) return;
  try {
    const n = await invoke<number>("clear_alerting_aggregations");
    aggMsg(`${t("alertingSaved")} (${n})`, true);
    void refreshAlertingAggregations();
  } catch (err) {
    aggMsg(String(err), false);
  }
}

// ─── Phase 55: alerting correlation suppression (B is suppressed within window_secs after A) ────────

interface CorrelationRuleDto {
  id: string;
  name: string;
  kindPatternA: string;
  kindPatternB: string;
  windowSecs: number;
  enabled: boolean;
  createdAt: number;
}

function corrMsg(text: string, ok: boolean): void {
  const msg = document.getElementById("corr-msg");
  if (msg) msg.textContent = `${ok ? "✓" : "✗"} ${text}`;
}

async function refreshAlertingCorrelations(): Promise<void> {
  const list = document.getElementById("alerting-correlations-list");
  if (!list) return;
  let rules: CorrelationRuleDto[] = [];
  try {
    rules = await invoke<CorrelationRuleDto[]>("list_alerting_correlations");
  } catch (err) {
    list.innerHTML = `<div class="setting-hint">✗ ${String(err)}</div>`;
    return;
  }
  if (rules.length === 0) {
    list.innerHTML = `<div class="setting-hint">${esc(t("alertingCorrelationsEmpty"))}</div>`;
    return;
  }
  list.innerHTML = rules
    .map((r) => {
      const enabledBadge = `<span class="corr-enabled-badge ${r.enabled ? "on" : "off"}">${r.enabled ? esc(t("alertingCorrelationEnabledOn")) : esc(t("alertingCorrelationEnabledOff"))}</span>`;
      const toggle = `<button class="btn ghost corr-toggle" data-id="${esc(r.id)}" type="button">${r.enabled ? esc(t("alertingCorrelationDisable")) : esc(t("alertingCorrelationEnable"))}</button>`;
      return `<div class="corr-card ${r.enabled ? "corr-card-on" : "corr-card-off"}" data-id="${esc(r.id)}">
        <span class="corr-name">${esc(r.name)}</span>
        <code class="corr-pattern-a">${esc(r.kindPatternA)}</code>
        <span class="corr-arrow">→</span>
        <code class="corr-pattern-b">${esc(r.kindPatternB)}</code>
        <span class="corr-window">≤ ${r.windowSecs}s</span>
        ${enabledBadge}
        ${toggle}
        <button class="btn ghost corr-delete" data-id="${esc(r.id)}" type="button">${esc(t("alertingCorrelationDelete"))}</button>
      </div>`;
    })
    .join("");
  list.querySelectorAll<HTMLButtonElement>(".corr-delete").forEach((btn) => {
    btn.addEventListener("click", () => void onDeleteCorrelation(btn.dataset.id || ""));
  });
  list.querySelectorAll<HTMLButtonElement>(".corr-toggle").forEach((btn) => {
    btn.addEventListener("click", () => void onToggleCorrelation(btn.dataset.id || ""));
  });
}

async function onAddCorrelation(): Promise<void> {
  const name = (document.getElementById("corr-name") as HTMLInputElement)?.value.trim() || "";
  const pa = (document.getElementById("corr-pattern-a") as HTMLInputElement)?.value.trim() || "";
  const pb = (document.getElementById("corr-pattern-b") as HTMLInputElement)?.value.trim() || "";
  const windowSecs = parseInt((document.getElementById("corr-window") as HTMLInputElement)?.value || "0", 10);
  if (!name) {
    corrMsg(t("alertingCorrelationNameRequired"), false);
    return;
  }
  if (windowSecs <= 0) {
    corrMsg(t("alertingCorrelationInvalid"), false);
    return;
  }
  try {
    const rule: CorrelationRuleDto = {
      id: "",
      name,
      kindPatternA: pa,
      kindPatternB: pb,
      windowSecs,
      enabled: true,
      createdAt: 0,
    };
    const saved = await invoke<CorrelationRuleDto>("save_alerting_correlation", { rule });
    corrMsg(`${t("alertingSaved")}: ${saved.name}`, true);
    const n = document.getElementById("corr-name") as HTMLInputElement | null;
    if (n) n.value = "";
    void refreshAlertingCorrelations();
  } catch (err) {
    corrMsg(String(err), false);
  }
}

async function onDeleteCorrelation(id: string): Promise<void> {
  if (!id) return;
  if (!window.confirm(t("alertingCorrelationDeleteConfirm"))) return;
  try {
    const ok = await invoke<boolean>("delete_alerting_correlation", { id });
    corrMsg(ok ? t("alertingSaved") : t("alertingCorrelationDeleteFailed"), ok);
    void refreshAlertingCorrelations();
  } catch (err) {
    corrMsg(String(err), false);
  }
}

async function onToggleCorrelation(id: string): Promise<void> {
  if (!id) return;
  try {
    const rules = await invoke<CorrelationRuleDto[]>("list_alerting_correlations");
    const r = rules.find((x) => x.id === id);
    if (!r) {
      corrMsg(t("alertingCorrelationNotFound"), false);
      return;
    }
    const updated: CorrelationRuleDto = { ...r, enabled: !r.enabled };
    await invoke<CorrelationRuleDto>("save_alerting_correlation", { rule: updated });
    corrMsg(`${t("alertingSaved")}: ${updated.name} (${updated.enabled ? "on" : "off"})`, true);
    void refreshAlertingCorrelations();
  } catch (err) {
    corrMsg(String(err), false);
  }
}

async function clearAllCorrelations(): Promise<void> {
  if (!window.confirm(t("alertingCorrelationClearConfirm"))) return;
  try {
    const n = await invoke<number>("clear_alerting_correlations");
    corrMsg(`${t("alertingSaved")} (${n})`, true);
    void refreshAlertingCorrelations();
  } catch (err) {
    corrMsg(String(err), false);
  }
}

// ─── Phase 56: alerting escalation chain ─────────────────────────────────

interface EscalationRuleDto {
  id: string;
  name: string;
  kindPattern: string;
  escalateAfterSecs: number;
  targetSeverity: string;
  targetEndpointIds?: string[];
  enabled: boolean;
  createdAt: number;
}

function escMsg(text: string, ok: boolean): void {
  const msg = document.getElementById("esc-msg");
  if (msg) msg.textContent = `${ok ? "✓" : "✗"} ${text}`;
}

async function refreshAlertingEscalations(): Promise<void> {
  const list = document.getElementById("alerting-escalations-list");
  if (!list) return;
  let rules: EscalationRuleDto[] = [];
  try {
    rules = await invoke<EscalationRuleDto[]>("list_alerting_escalations");
  } catch (err) {
    list.innerHTML = `<div class="setting-hint">✗ ${String(err)}</div>`;
    return;
  }
  if (rules.length === 0) {
    list.innerHTML = `<div class="setting-hint">${esc(t("alertingEscalationsEmpty"))}</div>`;
    return;
  }
  list.innerHTML = rules
    .map((r) => {
      const enabledBadge = `<span class="esc-enabled-badge ${r.enabled ? "on" : "off"}">${r.enabled ? esc(t("alertingEscalationEnabledOn")) : esc(t("alertingEscalationEnabledOff"))}</span>`;
      const toggle = `<button class="btn ghost esc-toggle" data-id="${esc(r.id)}" type="button">${r.enabled ? esc(t("alertingEscalationDisable")) : esc(t("alertingEscalationEnable"))}</button>`;
      const endpointsLabel = r.targetEndpointIds && r.targetEndpointIds.length > 0
        ? ` → ${esc(r.targetEndpointIds.join(", "))}`
        : ` → ${esc(t("alertingEscalationAllEndpoints"))}`;
      return `<div class="esc-card ${r.enabled ? "esc-card-on" : "esc-card-off"}" data-id="${esc(r.id)}">
        <span class="esc-name">${esc(r.name)}</span>
        <code class="esc-pattern">${esc(r.kindPattern)}</code>
        <span class="esc-window">≥ ${r.escalateAfterSecs}s</span>
        <span class="esc-target-severity">${esc(r.targetSeverity)}</span>
        <span class="esc-endpoints">${endpointsLabel}</span>
        ${enabledBadge}
        ${toggle}
        <button class="btn ghost esc-delete" data-id="${esc(r.id)}" type="button">${esc(t("alertingEscalationDelete"))}</button>
      </div>`;
    })
    .join("");
  list.querySelectorAll<HTMLButtonElement>(".esc-delete").forEach((btn) => {
    btn.addEventListener("click", () => void onDeleteEscalation(btn.dataset.id || ""));
  });
  list.querySelectorAll<HTMLButtonElement>(".esc-toggle").forEach((btn) => {
    btn.addEventListener("click", () => void onToggleEscalation(btn.dataset.id || ""));
  });
}

async function onAddEscalation(): Promise<void> {
  const name = (document.getElementById("esc-name") as HTMLInputElement)?.value.trim() || "";
  const pattern = (document.getElementById("esc-pattern") as HTMLInputElement)?.value.trim() || "";
  const afterSecs = parseInt((document.getElementById("esc-after") as HTMLInputElement)?.value || "0", 10);
  const severity = (document.getElementById("esc-target-severity") as HTMLSelectElement)?.value || "critical";
  const endpointsRaw = (document.getElementById("esc-endpoints") as HTMLInputElement)?.value.trim() || "";
  if (!name) {
    escMsg(t("alertingEscalationNameRequired"), false);
    return;
  }
  if (afterSecs <= 0) {
    escMsg(t("alertingEscalationInvalid"), false);
    return;
  }
  const endpointIds = endpointsRaw
    ? endpointsRaw.split(",").map((s) => s.trim()).filter((s) => s.length > 0)
    : undefined;
  try {
    const rule: EscalationRuleDto = {
      id: "",
      name,
      kindPattern: pattern,
      escalateAfterSecs: afterSecs,
      targetSeverity: severity,
      targetEndpointIds: endpointIds,
      enabled: true,
      createdAt: 0,
    };
    const saved = await invoke<EscalationRuleDto>("save_alerting_escalation", { rule });
    escMsg(`${t("alertingSaved")}: ${saved.name}`, true);
    const n = document.getElementById("esc-name") as HTMLInputElement | null;
    if (n) n.value = "";
    void refreshAlertingEscalations();
  } catch (err) {
    escMsg(String(err), false);
  }
}

async function onDeleteEscalation(id: string): Promise<void> {
  if (!id) return;
  if (!window.confirm(t("alertingEscalationDeleteConfirm"))) return;
  try {
    const ok = await invoke<boolean>("delete_alerting_escalation", { id });
    escMsg(ok ? t("alertingSaved") : t("alertingEscalationDeleteFailed"), ok);
    void refreshAlertingEscalations();
  } catch (err) {
    escMsg(String(err), false);
  }
}

async function onToggleEscalation(id: string): Promise<void> {
  if (!id) return;
  try {
    const rules = await invoke<EscalationRuleDto[]>("list_alerting_escalations");
    const r = rules.find((x) => x.id === id);
    if (!r) {
      escMsg(t("alertingEscalationNotFound"), false);
      return;
    }
    const updated: EscalationRuleDto = { ...r, enabled: !r.enabled };
    await invoke<EscalationRuleDto>("save_alerting_escalation", { rule: updated });
    escMsg(`${t("alertingSaved")}: ${updated.name} (${updated.enabled ? "on" : "off"})`, true);
    void refreshAlertingEscalations();
  } catch (err) {
    escMsg(String(err), false);
  }
}

async function clearAllEscalations(): Promise<void> {
  if (!window.confirm(t("alertingEscalationClearConfirm"))) return;
  try {
    const n = await invoke<number>("clear_alerting_escalations");
    escMsg(`${t("alertingSaved")} (${n})`, true);
    void refreshAlertingEscalations();
  } catch (err) {
    escMsg(String(err), false);
  }
}

// ─── Phase 68: Correlation — cycle linter + timeline ────────────────────

function cycleKindLabel(kind: CycleReportDto["kind"]): string {
  if (kind === "self_loop") return t("alertingCorrelationCycleKindSelf");
  if (kind === "route_to_route") return t("alertingCorrelationCycleKindRoute");
  return t("alertingCorrelationCycleKindCorrelation");
}

async function onDetectCycles(): Promise<void> {
  const msg = document.getElementById("alerting-correlation-msg");
  const out = document.getElementById("alerting-correlation-cycles-result");
  if (!out) return;
  try {
    const reports = await invoke<CycleReportDto[]>("detect_alerting_cycles");
    if (reports.length === 0) {
      out.innerHTML = `<span class="setting-hint">${esc(t("alertingCorrelationNoCycle"))}</span>`;
      if (msg) msg.textContent = "";
      return;
    }
    out.innerHTML = reports.map((r) => {
      const arrowed = r.cycle.map((n, i) => `${i > 0 ? " → " : ""}<code>${esc(n)}</code>`).join("");
      return `<div class="alerting-correlation-cycle"><strong>[${esc(cycleKindLabel(r.kind))}]</strong> ${arrowed}</div>`;
    }).join("");
    if (msg) msg.textContent = `${reports.length} cycle(s)`;
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}

async function onRefreshTimeline(): Promise<void> {
  const msg = document.getElementById("alerting-correlation-msg");
  const tl = document.getElementById("alerting-correlation-timeline");
  if (!tl) return;
  try {
    const events = await invoke<RouteSeenEventDto[]>("recent_alerting_events", { limit: 50 });
    if (events.length === 0) {
      tl.innerHTML = `<span class="setting-hint">${esc(t("alertingCorrelationTimelineEmpty"))}</span>`;
      if (msg) msg.textContent = "";
      return;
    }
    const now = Math.floor(Date.now() / 1000);
    tl.innerHTML = events.map((e) => {
      const ago = now - e.tsSecs;
      const ts = ago < 5 ? t("alertingCorrelationTimelineNow") : `${ago}${t("alertingCorrelationTimelineSecsAgo")}`;
      const fired = e.routesFired.length > 0
        ? ` <span class="routes-fired">${esc(t("alertingCorrelationTimelineRoutesFired"))} ${e.routesFired.map(esc).join(", ")}</span>`
        : "";
      const corr = e.correlationsHit.length > 0
        ? ` <span class="corr-hit">[${esc(t("alertingCorrelationTimelineCorrelationsHit"))} ${e.correlationsHit.map(esc).join(", ")}]</span>`
        : "";
      return `<div class="alerting-correlation-timeline-event"><span class="ts">${esc(ts)}</span> <code>${esc(e.source)}</code> <span class="payload-summary">${esc(e.payloadSummary)}</span>${fired}${corr}</div>`;
    }).join("");
    if (msg) msg.textContent = `${events.length} event(s)`;
  } catch (err) {
    if (msg) msg.textContent = String(err);
  }
}



