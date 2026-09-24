import "./settings.css";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { isEnabled } from "@tauri-apps/plugin-autostart";
import { open } from "@tauri-apps/plugin-dialog";
import DOMPurify from "dompurify";
import { marked } from "marked";
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
  connectEventStream,
  disconnectEventStream,
  onEvent,
  type OpencapxEvent,
} from "./events";
import {
  BUBBLE_DENSITIES,
  BUBBLE_POSITIONS,
  BUBBLE_THEMES,
  type BubblePos,
  type BubbleTheme,
} from "./bubble";
import {
  DEFAULTS,
  applyTheme,
  bindToggles,
  clampInt,
  cssEscape,
  dayLabel,
  detailBlock,
  detailKv,
  detailPayload,
  esc,
  escAttr,
  formatBytes,
  formatLifecycleTime,
  getAppVersion,
  getPluginNameById,
  getPluginPageReturn,
  getSettings,
  getTab,
  group,
  listError,
  listSkeleton,
  load,
  loadDbRecoveryNotice,
  openPluginSettingsPage,
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
  setPluginNameById,
  setSettings,
  setWindowFocused,
  startThemeListener,
  switchTab,
  toggle,
  type Tab,
} from "./settings/shared";
import type {
  Cond,
  HotkeyAction,
  LocalizedText,
  PaletteEntry,
  PluginConfigRow,
  PluginSettingsView,
  SettingDecl,
  ValidateRule,
} from "./settings/types";
import { renderBackup } from "./settings/backup";
import { renderNotify } from "./settings/notify";
import { renderProfilesTab, stopWorkspaceListener } from "./settings/profiles";
import { renderStats } from "./settings/stats";
import { renderSla } from "./settings/sla";

let auditStreamOnEvent: (() => void) | null = null;
// i2 §14 — the Audit tab shows the Activity Timeline by default; the old permission list moves into a second view.
let auditView: "timeline" | "perms" = "timeline";

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

/// Global policy entry (core_permission_list). Control granularity = permission, not capability.
interface CorePermPolicy {
  permission: string;
  capabilities: string[];
  builtinDefault: string;
  /// null = follow the built-in default
  override: string | null;
  effective: string;
  highRisk: boolean;
}

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


  } else if (getTab() === "bubble") {
    // Themes changed to a palette grid: 10 themes can't be chosen by name alone, you need to see the colors
    const THEME_I18N: Record<BubbleTheme, I18nKey> = {
      chef: "bubbleThemeChef",
      engineer: "bubbleThemeEngineer",
      wizard: "bubbleThemeWizard",
      explorer: "bubbleThemeExplorer",
      scientist: "bubbleThemeScientist",
      minimal: "bubbleThemeMinimal",
      paper: "bubbleThemePaper",
      cyber: "bubbleThemeCyber",
      terminal: "bubbleThemeTerminal",
      pixel: "bubbleThemePixel",
      manga: "bubbleThemeManga",
      blueprint: "bubbleThemeBlueprint",
    };
    const MODE_I18N: Record<string, I18nKey> = {
      list: "modeList",
      carousel: "modeCarousel",
      compact: "modeCompact",
      focus: "modeFocus",
    };
    const POS_I18N: Record<BubblePos, I18nKey> = {
      right: "bubblePosRight",
      left: "bubblePosLeft",
      top: "bubblePosTop",
      bottom: "bubblePosBottom",
    };
    const DENSITY_I18N: Record<string, I18nKey> = {
      tight: "densityTight",
      standard: "densityStandard",
      rich: "densityRich",
    };
    // Palettes no longer hardcode colors: the preview block carries data-bubble-theme, and bubble-themes.css renders it
    // with the same colors + shapes as the desktop bubble (corner radius/bevel/border/texture/shadow).
    const themeGrid = `<div class="bubble-theme-grid">${BUBBLE_THEMES.map((th) => {
      const on = th === getSettings().bubbleTheme;
      return `<button class="bubble-theme${on ? " active" : ""}" data-theme="${th}" type="button" aria-pressed="${on}">
        <span class="bubble-theme-swatch" data-bubble-theme="${th}">
          <i class="bse"></i><i class="bsl"></i><i class="bsl s"></i>
        </span>
        <span class="bubble-theme-name">${esc(t(THEME_I18N[th]))}</span>
      </button>`;
    }).join("")}</div>`;
    const posPicker = `<div class="bubble-pos-picker">${BUBBLE_POSITIONS.map((p) => {
      const on = p === getSettings().bubblePos;
      return `<button class="bubble-pos${on ? " active" : ""}" data-pos="${p}" type="button" aria-pressed="${on}" title="${esc(t(POS_I18N[p]))}">
        <span class="bubble-pos-dia" data-dia="${p}"><i class="bubble-pos-pet"></i><i class="bubble-pos-bub"></i></span>
        <span class="bubble-pos-label">${esc(t(POS_I18N[p]))}</span>
      </button>`;
    }).join("")}</div>`;
    // The grid needs the full row width (10 swatches), so this row is stacked instead of the usual
    // label-left / control-right: sharing one line crushes the hint into a sliver.
    const themeRow = `<div class="setting-row vertical"><div class="setting-info"><span class="setting-label">${esc(t("bubbleTheme"))}</span><span class="setting-hint">${esc(t("bubbleThemeHint"))}</span></div>${themeGrid}</div>`;
    body.innerHTML = group("tabBubble",
      row("bubbleEnable", "bubbleEnableHint", toggle("bubbleEnabled", getSettings().bubbleEnabled)) +
      row("mode", "modeHint", segmented("mode", ["list", "carousel", "compact", "focus"], getSettings().mode, MODE_I18N)) +
      row("bubblePos", "bubblePosHint", posPicker) +
      row("bubbleGap", "bubbleGapHint", `<input type="range" id="bgap" min="0" max="24" value="${getSettings().bubbleGap}" /><span id="bgap-v" class="pet-num">${getSettings().bubbleGap}px</span>`) +
      themeRow +
      row("density", "densityHint", segmented("bubbleDensity", [...BUBBLE_DENSITIES], getSettings().bubbleDensity, DENSITY_I18N)) +
      row("maxRows", "maxRowsHint", `<input type="number" id="brows" min="1" max="10" value="${getSettings().maxRows}" />`) +
      row("bubbleDuration", "bubbleDurationHint", `<input type="number" id="bdur" min="0" max="300" value="${getSettings().bubbleDuration}" />`));
    body.querySelectorAll<HTMLButtonElement>(".bubble-pos").forEach((b) => {
      b.addEventListener("click", () => {
        setSettings({ ...getSettings(), bubblePos: b.dataset.pos ?? "right" });
        void save();
        render();
      });
    });
    const bgap = document.getElementById("bgap") as HTMLInputElement | null;
    bgap?.addEventListener("input", () => {
      setSettings({ ...getSettings(), bubbleGap: Number(bgap.value) });
      document.getElementById("bgap-v")!.textContent = `${bgap.value}px`;
      void save();
    });
    body.querySelectorAll<HTMLButtonElement>(".bubble-theme").forEach((b) => {
      b.addEventListener("click", () => {
        setSettings({ ...getSettings(), bubbleTheme: b.dataset.theme ?? "chef" });
        void save();
        render();
      });
    });
    body.querySelectorAll("button[data-seg='mode']").forEach((b) => {
      b.addEventListener("click", () => {
        setSettings({ ...getSettings(), mode: (b as HTMLElement).dataset.val ?? "carousel" });
        void save();
        render();
      });
    });
    body.querySelectorAll("button[data-seg='bubbleDensity']").forEach((b) => {
      b.addEventListener("click", () => {
        setSettings({ ...getSettings(), bubbleDensity: (b as HTMLElement).dataset.val ?? "standard" });
        void save();
        render();
      });
    });
    document.getElementById("brows")?.addEventListener("change", (e) => {
      const input = e.target as HTMLInputElement;
      const v = clampInt(input.value, 1, 10, DEFAULTS.maxRows);
      input.value = String(v); // show the value that actually takes effect
      setSettings({ ...getSettings(), maxRows: v });
      void save();
    });
    document.getElementById("bmins")?.addEventListener("change", (e) => {
      const input = e.target as HTMLInputElement;
      const v = clampInt(input.value, 5, 480, DEFAULTS.breakMinutes);
      input.value = String(v);
      setSettings({ ...getSettings(), breakMinutes: v });
      void save();
    });
    document.getElementById("bdur")?.addEventListener("change", (e) => {
      const input = e.target as HTMLInputElement;
      const v = clampInt(input.value, 0, 300, DEFAULTS.bubbleDuration);
      input.value = String(v);
    });
  } else if (getTab() === "plugins") {
    body.innerHTML =
      group("tabPlugins",
        `<label class="setting-row plugin-policy-row"><div class="setting-info"><span class="setting-label">${esc(t("pluginAllowUnsigned"))}</span><span class="setting-hint">${esc(t("pluginsHint"))}</span></div><input type="checkbox" id="allow-unsigned"/></label>
        <label class="setting-row plugin-policy-row"><div class="setting-info"><span class="setting-label">${esc(t("pluginSandboxEnforcement"))}</span></div><input type="checkbox" id="sandbox-enforcement"/></label>
        <div class="setting-row plugin-install-row"><button class="btn" id="plugin-install" type="button">${esc(t("pluginInstall"))}</button><span class="setting-hint" id="plugin-install-msg"></span></div>`)
      + `<div id="plugin-list" class="plugin-list"></div>`
      + group(null,
        `<div class="setting-row vertical"><span class="setting-hint">${esc(t("depGraphHint"))}</span><div><button class="btn ghost" id="dep-refresh" type="button">${esc(t("depGraphRefresh"))}</button><span class="setting-hint" id="dep-msg"></span></div><div id="dep-graph"></div></div>`)
      + group(null,
        `<div class="setting-row vertical"><span class="setting-hint">${esc(t("lifecyclePlanHint"))}</span><div><button class="btn ghost" id="lifecycle-refresh" type="button">${esc(t("lifecycleRefresh"))}</button><button class="btn ghost" id="lifecycle-start-all" type="button">${esc(t("lifecycleStartAll"))}</button><button class="btn ghost" id="lifecycle-stop-all" type="button">${esc(t("lifecycleStopAll"))}</button><span class="setting-hint" id="lifecycle-msg"></span></div><div id="lifecycle-plan"></div></div>`)
      + group(null,
        `<div class="setting-row vertical"><span class="setting-hint">${esc(t("heatmapHint"))}</span><div><button class="btn ghost" id="heatmap-refresh" type="button">${esc(t("heatmapRefresh"))}</button><span class="setting-hint" id="heatmap-msg"></span></div><div id="heatmap-grid"></div><div class="stat-sub">${esc(t("heatmapTopDenied"))}</div><div id="heatmap-top"></div></div>`);
    document.getElementById("plugin-install")?.addEventListener("click", () => void pickAndInstallPlugin());
    void initAllowUnsigned();
    void initSandboxEnforcement();
    document.getElementById("dep-refresh")?.addEventListener("click", () => void refreshDepGraph());
    document.getElementById("heatmap-refresh")?.addEventListener("click", () => void refreshHeatmap());
    document.getElementById("lifecycle-refresh")?.addEventListener("click", () => void refreshLifecyclePlan());
    document.getElementById("lifecycle-start-all")?.addEventListener("click", () => void onStartAllPlugins());
    document.getElementById("lifecycle-stop-all")?.addEventListener("click", () => void onStopAllPlugins());
    void refreshPlugins();
    void refreshDepGraph();
    void refreshHeatmap();
    void refreshLifecyclePlan();
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
  } else if (getTab() === "market") {
    body.innerHTML = group("tabMarket",
      `<div class="setting-row vertical"><span class="setting-hint">${esc(t("marketHint"))}</span><div><button class="btn ghost" id="market-refresh" type="button">${esc(t("marketRefresh"))}</button><span class="setting-hint" id="market-msg"></span></div><div id="market-list"></div></div>`);
    document.getElementById("market-refresh")?.addEventListener("click", () => void refreshMarket(true));
    void refreshMarket(false);
  } else if (getTab() === "agents") {
    body.innerHTML = group("tabAgents",
      `<div class="setting-row vertical"><span class="setting-hint">${esc(t("agentsViewHint"))}</span><div class="setting-hint">${esc(t("agentsPermHighRiskHint"))}</span><div><button class="btn ghost" id="agents-id-refresh" type="button">${esc(t("auditRefresh"))}</button><span class="setting-hint" id="agents-id-msg"></span></div><div id="agents-id-list"></div></div>`);
    document.getElementById("agents-id-refresh")?.addEventListener("click", () => void refreshIdAgents());
    void refreshIdAgents();
  } else if (getTab() === "rpcTrace") {
    // Request chains: grouped by project, all expanded inline (project bodies come with the full list in one shot), no dialogs.
    body.innerHTML = group("rpcTraceTitle",
      `<div class="setting-row vertical"><span class="setting-hint">${esc(t("rpcTraceTitle"))} · ${esc(t("rpcHookSessions"))}</span><div><button class="btn ghost" id="rpc-trace-refresh" type="button">${esc(t("auditRefresh"))}</button><button class="btn ghost" id="rpc-export-all-projects" type="button">${esc(t("rpcExportAllProjects"))}</button><span class="setting-hint" id="rpc-trace-msg"></span></div><div id="rpc-trace-list" class="rpc-rows"></div></div>`);
    document.getElementById("rpc-trace-refresh")?.addEventListener("click", () => void refreshRpcTraces());
    // Look up the button by id at click time (a tab switch rebuilds this row); don't capture stale references
    document.getElementById("rpc-export-all-projects")?.addEventListener("click", () => {
      const btn = document.getElementById("rpc-export-all-projects") as HTMLButtonElement | null;
      if (btn) void runRpcExportAllProjects(btn);
    });
    // Only here is the enter animation allowed; the refresh button and failure retry use the default (allowEnter=false) and automatically get .rpc-no-enter
    void refreshRpcTraces(true);
  } else if (getTab() === "capabilities") {
    body.innerHTML = group("tabCapabilities", capLegend())
      + `<div id="core-perm-list"></div>`;
    document.getElementById("core-perm-refresh")?.addEventListener("click", () => void refreshCorePerms());
    void refreshCorePerms(null, true);
  } else if (getTab() === "audit") {
    body.innerHTML = group("tabAudit",
      `<div class="setting-row vertical"><div class="audit-view-toggle"><button class="btn ghost" id="audit-view-timeline" type="button" aria-pressed="${auditView === "timeline"}">${esc(t("auditViewTimeline"))}</button><button class="btn ghost" id="audit-view-perms" type="button" aria-pressed="${auditView === "perms"}">${esc(t("auditViewPerms"))}</button></div></div>
      <div class="setting-row vertical" id="timeline-row"${auditView === "perms" ? " hidden" : ""}><div class="filter-bar"><label class="filter-field filter-field-narrow"><span class="filter-label">${esc(t("timelineAgentPlaceholder"))}</span><input type="text" id="timeline-agent" placeholder="${esc(t("timelineAgentPlaceholder"))}" aria-label="${esc(t("timelineAgentPlaceholder"))}" /></label><div class="filter-actions"><button class="btn ghost" id="timeline-refresh" type="button">${esc(t("auditRefresh"))}</button><span class="setting-hint" id="timeline-msg"></span></div></div><div id="timeline-list"></div></div>
      <div class="setting-row vertical" id="audit-row"${auditView === "timeline" ? " hidden" : ""}><span class="setting-hint">${esc(t("auditHint"))}</span><div class="filter-bar"><label class="filter-field"><span class="filter-label">${esc(t("auditSearchLabel"))}</span><input type="text" id="audit-q" placeholder="${esc(t("auditSearchPlaceholder"))}" aria-label="${esc(t("auditSearchLabel"))}" /></label><div class="filter-actions"><button class="btn ghost" id="audit-apply" type="button">${esc(t("auditApply"))}</button><button class="btn ghost" id="audit-refresh" type="button">${esc(t("auditRefresh"))}</button></div></div><details class="filter-adv" id="audit-adv"><summary class="filter-adv-summary">${esc(t("auditAdvanced"))}<span class="filter-adv-mark" id="audit-adv-mark"></span></summary><div class="filter-grid"><label class="filter-field"><span class="filter-label">${esc(t("auditPrefixLabel"))}</span><input type="text" id="audit-prefix" placeholder="${esc(t("auditPrefixPlaceholder"))}" aria-label="${esc(t("auditPrefixLabel"))}" /></label><label class="filter-field"><span class="filter-label">${esc(t("auditSince"))}</span><input type="datetime-local" id="audit-since" aria-label="${esc(t("auditSince"))}" /></label><label class="filter-field"><span class="filter-label">${esc(t("auditUntil"))}</span><input type="datetime-local" id="audit-until" aria-label="${esc(t("auditUntil"))}" /></label></div><div class="filter-actions"><button class="btn ghost" id="audit-clear" type="button">${esc(t("auditClear"))}</button><button class="btn ghost" id="audit-export" type="button">${esc(t("auditExportCsv"))}</button></div></details><div class="filter-status"><span class="setting-hint" id="audit-msg"></span><span class="audit-live" id="audit-live">${esc(t("auditOffline"))}</span></div><div id="audit-list"></div></div>
      <div class="setting-row vertical"><span class="setting-hint">${esc(t("replayHint"))}</span><div><button class="btn ghost" id="replay-refresh" type="button">${esc(t("replayRefresh"))}</button><span class="setting-hint" id="replay-msg"></span></div><div id="replay-list"></div></div>`);
    document.getElementById("audit-view-timeline")?.addEventListener("click", () => setAuditView("timeline"));
    document.getElementById("audit-view-perms")?.addEventListener("click", () => setAuditView("perms"));
    document.getElementById("timeline-refresh")?.addEventListener("click", () => void refreshTimeline());
    document.getElementById("timeline-agent")?.addEventListener("keydown", (ev) => {
      if ((ev as KeyboardEvent).key === "Enter") void refreshTimeline();
    });
    document.getElementById("audit-refresh")?.addEventListener("click", () => void refreshAudit());
    document.getElementById("audit-apply")?.addEventListener("click", () => void refreshAudit());
    document.getElementById("audit-q")?.addEventListener("keydown", (ev) => {
      if ((ev as KeyboardEvent).key === "Enter") void refreshAudit();
    });
    document.getElementById("audit-clear")?.addEventListener("click", () => {
      (document.getElementById("audit-q") as HTMLInputElement | null)!.value = "";
      (document.getElementById("audit-prefix") as HTMLInputElement | null)!.value = "";
      (document.getElementById("audit-since") as HTMLInputElement | null)!.value = "";
      (document.getElementById("audit-until") as HTMLInputElement | null)!.value = "";
      paintAuditAdv();
      void refreshAudit();
    });
    document.getElementById("audit-export")?.addEventListener("click", () => void exportAuditCsv());
    // Even when advanced filters are collapsed, it must be visible that 'a filter is active': any prefix/time value -> the badge shows a count.
    // The search box is always visible so it doesn't count; values still come from readAuditFilter(), without changing refreshAudit logic.
    const paintAuditAdv = (): void => {
      const mark = document.getElementById("audit-adv-mark");
      if (!mark) return;
      const f = readAuditFilter();
      const n = (f.kindPrefix ? 1 : 0) + (f.sinceTs ? 1 : 0) + (f.untilTs ? 1 : 0);
      mark.textContent = n > 0 ? `• ${n}` : "";
    };
    ["audit-prefix", "audit-since", "audit-until"].forEach((id) =>
      document.getElementById(id)?.addEventListener("input", paintAuditAdv),
    );
    paintAuditAdv();
    document.getElementById("replay-refresh")?.addEventListener("click", () => void refreshReplay());
    if (auditView === "timeline") void refreshTimeline();
    else void refreshAudit();
    void refreshReplay();
    startAuditStream();
  } else if (getTab() === "automation") {
    // i2 §15 Automation: Event -> Rule -> Action; rules land in ~/.opencapx/automation.json (see docs/automation.md)
    // The form goes into a dialog: the tab keeps only 'Add rule' + count/errors + the rule list, so the list owns the main surface.
    body.innerHTML = group("tabAutomation",
      `<div class="setting-row vertical"><span class="setting-hint">${esc(t("automationHint"))}</span><div class="rule-toolbar"><button class="btn" id="auto-add" type="button">${esc(t("automationAdd"))}</button><span class="setting-hint" id="automation-msg"></span></div><div id="automation-list"></div></div>`);
    document.getElementById("auto-add")?.addEventListener("click", () => openAutomationDialog());
    void refreshAutomation();
  } else if (getTab() === "rules") {
    // Command rules: route an agent's shell command to a specified executor (see docs/rules.md).
    // The add form is inline in the tab (no dialog); project-level rules are read-only, their source stays in the repo file.
    // Three parts: 1) add rule (form) 2) existing rules (list) 3) trusted projects (trust management).
    body.innerHTML = `<div class="rules-page">
      <p class="rules-intro">${esc(t("rulesHint"))}</p>
      <section class="rules-sect">
        <p class="rules-sect-title">${esc(t("rulesAdd"))}</p>
        <div class="rules-form">
          <label class="rules-field"><span class="rules-label">${esc(t("rulesMatchLabel"))}</span><select id="rule-match-kind" aria-label="${escAttr(t("rulesMatchLabel"))}"><option value="prefix">${esc(t("rulesMatchPrefix"))}</option><option value="binary">${esc(t("rulesMatchBinary"))}</option><option value="regex">${esc(t("rulesMatchRegex"))}</option></select></label>
          <label class="rules-field"><span class="rules-label">${esc(t("rulesMatchValuePlaceholder"))}</span><input type="text" id="rule-match-value" aria-label="${escAttr(t("rulesMatchValuePlaceholder"))}" /></label>
          <label class="rules-field"><span class="rules-label">${esc(t("rulesXformLabel"))}</span><select id="rule-xform-kind" aria-label="${escAttr(t("rulesXformLabel"))}"><option value="prepend">${esc(t("rulesXformPrepend"))}</option><option value="replace_binary">${esc(t("rulesXformReplace"))}</option><option value="env">${esc(t("rulesXformEnv"))}</option></select></label>
          <div class="rules-xform">
            <label class="rules-field" id="rule-f-prepend"><span class="rules-label">${esc(t("rulesPrependPlaceholder"))}</span><input type="text" id="rule-prepend" aria-label="${escAttr(t("rulesPrependPlaceholder"))}" /></label>
            <label class="rules-field" id="rule-f-from" hidden><span class="rules-label">${esc(t("rulesFromPlaceholder"))}</span><input type="text" id="rule-from" aria-label="${escAttr(t("rulesFromPlaceholder"))}" /></label>
            <label class="rules-field" id="rule-f-to" hidden><span class="rules-label">${esc(t("rulesToPlaceholder"))}</span><input type="text" id="rule-to" aria-label="${escAttr(t("rulesToPlaceholder"))}" /></label>
            <label class="rules-field" id="rule-f-envk" hidden><span class="rules-label">${esc(t("rulesEnvKeyPlaceholder"))}</span><input type="text" id="rule-env-k" aria-label="${escAttr(t("rulesEnvKeyPlaceholder"))}" /></label>
            <label class="rules-field" id="rule-f-envv" hidden><span class="rules-label">${esc(t("rulesEnvValuePlaceholder"))}</span><input type="text" id="rule-env-v" aria-label="${escAttr(t("rulesEnvValuePlaceholder"))}" /></label>
          </div>
          <label class="rules-field rules-field-wide"><span class="rules-label">${esc(t("rulesIdOptional"))}</span><input type="text" id="rule-id" aria-label="${escAttr(t("rulesIdOptional"))}" /></label>
        </div>
        <div class="rules-actions"><button class="btn primary" id="rule-save" type="button">${esc(t("rulesAdd"))}</button><span class="rules-msg" id="rule-msg" role="status"></span></div>
      </section>
      <section class="rules-sect">
        <div class="rules-sect-head"><p class="rules-sect-title">${esc(t("rulesListTitle"))}</p><span class="rules-count" id="rules-msg"></span></div>
        <div id="rules-list" class="rules-list"></div>
      </section>
      <section class="rules-sect">
        <p class="rules-sect-title">${esc(t("rulesTrustTitle"))}</p>
        <p class="rules-sect-hint">${esc(t("rulesTrustHint"))}</p>
        <div class="rules-trust-form"><input class="rules-input" id="rules-trust-path" placeholder="${escAttr(t("rulesTrustPlaceholder"))}" aria-label="${escAttr(t("rulesTrustPlaceholder"))}" type="text"/><button class="btn ghost" id="rules-trust-add" type="button">${esc(t("rulesTrustAdd"))}</button></div>
        <span class="rules-msg" id="rules-trust-msg" role="status"></span>
        <div id="rules-trust-list" class="rules-trust-list"></div>
      </section>
    </div>`;
    document.getElementById("rule-save")?.addEventListener("click", () => void submitRuleForm());
    document.getElementById("rule-xform-kind")?.addEventListener("change", () => syncRuleXformFields());
    document.getElementById("rules-trust-add")?.addEventListener("click", () => void addTrustedProject());
    syncRuleXformFields();
    void refreshRules();
  } else if (getTab() === "logs") {
    body.innerHTML = group("tabLogs",
      `<div class="setting-row vertical"><span class="setting-hint">${esc(t("logsHint"))}</span><div><span class="audit-live" id="logs-live">${esc(t("logsOffline"))}</span></div><div id="logs-list"></div></div>
      <div class="setting-row vertical"><div class="logs-filter-row"><input class="logs-input" id="logs-q" placeholder="${esc(t("logsQueryPlaceholder"))}" type="text"/><select class="logs-select" id="logs-level"><option value="">${esc(t("logsLevelAll"))}</option><option value="info">${esc(t("logLevelInfo"))}</option><option value="warn">${esc(t("logLevelWarn"))}</option><option value="error">${esc(t("logLevelError"))}</option><option value="debug">${esc(t("logLevelDebug"))}</option></select><input class="logs-input" id="logs-plugin" placeholder="${esc(t("logsPluginPlaceholder"))}" type="text"/><label class="logs-tail-label"><input type="checkbox" id="logs-tail"/><span>${esc(t("logsTail"))}</span></label><button class="btn ghost" id="logs-apply" type="button">${esc(t("logsApply"))}</button><button class="btn ghost" id="logs-clear" type="button">${esc(t("logsClear"))}</button></div><span class="setting-hint" id="logs-msg"></span></div>`);
    document.getElementById("logs-apply")?.addEventListener("click", () => void refreshLogs(true));
    document.getElementById("logs-clear")?.addEventListener("click", () => clearLogFilters());
    document.getElementById("logs-tail")?.addEventListener("change", () => void onTailToggle());
    void refreshLogs(true);
    startLogsStream();
  } else if (getTab() === "hotkeys") {
    body.innerHTML = group("tabHotkeys",
      `<div class="setting-row vertical"><span class="setting-hint">${esc(t("hotkeysHint"))}</span><div><button class="btn ghost" id="hotkey-restore-defaults" type="button">${esc(t("hotkeyRestoreDefaults"))}</button><button class="btn ghost" id="hotkey-refresh" type="button">${esc(t("auditRefresh"))}</button><span class="setting-hint" id="hotkey-msg"></span></div><div id="hotkey-list"></div></div>`);
    document.getElementById("hotkey-restore-defaults")?.addEventListener("click", () => void restoreDefaultHotkeys());
    document.getElementById("hotkey-refresh")?.addEventListener("click", () => void refreshHotkeys());
    void refreshHotkeys();
    startHotkeyPaletteListener();
  } else if (getTab() === "metrics") {
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
        ? `<button class="btn ghost" data-agent-reauth="${esc(a.agent_id)}" type="button">${esc(t("agentsReauthorize"))}</button>`
        : `<button class="btn ghost danger" data-agent-revoke="${esc(a.agent_id)}" data-agent-name="${esc(a.displayName)}" type="button">${esc(t("agentsRevoke"))}</button>`;
      const permRows = perms
        .map((e) => {
          // docs/permission-domains.md §4.3 enforcement point 3: declared derived permissions offer only ask/denied
          // (Core's set_decision also rejects granted; this just avoids a pointless click)
          const noAlways = e.high_risk || e.declared;
          const opts = noAlways && e.decision !== "granted"
            ? [["ask", t("permAsk")], ["denied", t("permDenied")]]
            : [["granted", t("permGranted")], ["ask", t("permAsk")], ["denied", t("permDenied")]];
          const sel = `<select data-agent-perm="${esc(a.agent_id)}" data-perm="${esc(e.permission)}">${opts
            .map(([v, l]) => `<option value="${esc(v)}"${v === e.decision ? " selected" : ""}>${esc(l)}</option>`)
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
        `<details data-agent-details="${esc(a.agent_id)}"${openIds.has(a.agent_id) ? " open" : ""}><summary class="setting-label">${esc(t("agentsPermsTitle"))}</summary><div class="settings-list">${permRows}</div></details>` +
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

/// Global policy category table (array order = render order); permissions not listed fall into capCatOther, rendered last.
const CORE_PERM_CATEGORIES: Array<{ key: string; titleKey: I18nKey; permissions: string[] }> = [
  { key: "clipboard", titleKey: "capCatClipboard", permissions: ["clipboard.read", "clipboard.write"] },
  { key: "file", titleKey: "capCatFile", permissions: ["file.read", "filesystem.write"] },
  { key: "image", titleKey: "capCatImage", permissions: ["image.read"] },
  { key: "screen", titleKey: "capCatScreen", permissions: ["screen.capture"] },
  { key: "audio", titleKey: "capCatAudio", permissions: ["notification.post", "audio.output", "media.control"] },
  { key: "browser", titleKey: "capCatBrowser", permissions: ["browser.control"] },
  { key: "automation", titleKey: "capCatAutomation", permissions: ["automation.control", "input.control"] },
  { key: "pim", titleKey: "capCatPim", permissions: ["photos.read", "contacts.read", "calendar.read", "notes.read", "reminders.read", "reminders.write", "mail.read", "messages.read"] },
  { key: "location", titleKey: "capCatLocation", permissions: ["location.read"] },
  { key: "system", titleKey: "capCatSystem", permissions: ["system.settings", "power.control", "url.scheme.open"] },
  { key: "printer", titleKey: "capCatPrinter", permissions: ["printer.control"] },
  { key: "window", titleKey: "capCatWindow", permissions: ["window.management"] },
  { key: "things", titleKey: "capCatThings", permissions: ["things.read", "things.write"] },
];

/// Backend decision enum -> localized text (granted/ask/denied); unknown values are returned as-is.
function decisionLabel(v: string): string {
  if (v === "granted") return t("permGranted");
  if (v === "ask") return t("permAsk");
  if (v === "denied") return t("permDenied");
  return v;
}

/// Legend: tier explanation + hard-gate rule (red dot and red text, read as a warning rather than a gray hint) + subscription note + refresh.
function capLegend(): string {
  return `<div class="setting-row vertical cap-legend"><span class="cap-legend-title">${esc(t("corePermHint"))}</span><span class="cap-legend-rule">${esc(t("corePermHardGateHint"))}</span><span class="setting-hint">${esc(t("corePermSubscriptionNote"))}</span><div><button class="btn ghost" id="core-perm-refresh" type="button">${esc(t("auditRefresh"))}</button><span class="setting-hint" id="core-perm-msg"></span></div></div>`;
}

/// Single row: permission name + covered capability chip + built-in default tier + 4-tier native select.
/// High-risk permissions offer no granted option (the backend rejects global granted; this avoids a pointless click).
/// data-override for CSS scanning: '' = follow default; other tiers show a colored bar on the row's left edge;
/// data-cur colors the collapsed select. aria-label carries the permission name, so screen readers don't just say 'follow default'.
function corePermRow(e: CorePermPolicy): string {
  const current = e.override ?? "";
  const opts: Array<[string, I18nKey]> = [["", "corePermFollowDefault"]];
  if (!e.highRisk) opts.push(["granted", "permGranted"]);
  opts.push(["ask", "permAsk"]);
  opts.push(["denied", "permDenied"]);
  const sel = `<select data-cap-perm="${esc(e.permission)}" data-cur="${current}" aria-label="${esc(e.permission)}">${opts
    .map(([v, k]) => `<option value="${esc(v)}"${v === current ? " selected" : ""}>${esc(t(k))}</option>`)
    .join("")}</select>`;
  const chips = e.capabilities.map((c) => `<span class="cap-badge">${esc(c)}</span>`).join("");
  const chipsRow = e.capabilities.length > 0 ? `<span class="cap-chips">${chips}</span>` : "";
  const badge = e.highRisk ? ` <span class="cap-flag">${esc(t("permHighRisk"))}</span>` : "";
  // Rows whose effective tier is denied get a red dot: rows whose built-in default is denied were previously indistinguishable from ask rows; purely decorative, the semantics are carried by the text
  const gateDot = e.effective === "denied" ? `<span class="cap-dot" data-state="denied" aria-hidden="true"></span>` : "";
  const def = `<span class="cap-def" data-state="${esc(e.effective)}">${esc(decisionLabel(e.builtinDefault))}</span>`;
  return `<div class="setting-row cap-row" role="listitem" data-override="${current}"><div class="setting-info"><span class="setting-label cap-name">${gateDot}${esc(e.permission)}${badge}</span>${chipsRow}<span class="setting-hint">${esc(t("corePermBuiltinDefault"))}: ${def}</span></div><div class="perm-controls">${sel}</div></div>`;
}

/// Category-level bulk tier: a native select that reads the unified tier of that category's rows and writes back the selected state.
/// When rows have inconsistent tiers, show a disabled 'Mixed' placeholder — it is a status hint, not something selectable as an action.
function capBulkGroup(key: string, titleKey: I18nKey, rows: CorePermPolicy[]): string {
  const vals = new Set(rows.map((r) => r.override ?? ""));
  const mixed = vals.size > 1;
  const uniform = mixed ? "__mixed__" : ([...vals][0] ?? "");
  const opts: string[] = [];
  if (mixed) opts.push(`<option value="__mixed__" disabled selected>${esc(t("corePermMixed"))}</option>`);
  const opt = (val: string, labelKey: I18nKey): string =>
    `<option value="${esc(val)}"${!mixed && val === uniform ? " selected" : ""}>${esc(t(labelKey))}</option>`;
  opts.push(opt("", "corePermFollowDefault"));
  opts.push(opt("granted", "permGranted"));
  opts.push(opt("ask", "permAsk"));
  opts.push(opt("denied", "permDenied"));
  return `<div class="cap-bulk"><span class="setting-hint">${esc(t("corePermBulk"))}</span><select data-cap-bulk="${esc(key)}" data-cur="${esc(uniform)}" aria-label="${esc(t(titleKey))} · ${esc(t("corePermBulk"))}">${opts.join("")}</select></div>`;
}

/// Category card: one header row (category name + count + bulk tier), with the nested permission surface below (concentric radius 12 - 4 = 8).
/// The category name uses h3 + section aria-labelledby to enter the accessibility tree; all styling is still handled by .cap-cat-name.
function capCategory(g: { key: string; titleKey: I18nKey; rows: CorePermPolicy[] }): string {
  const headId = `capcat-${esc(g.key)}`;
  return `<section class="cap-cat" aria-labelledby="${headId}"><header class="cap-cat-head"><h3 class="cap-cat-name" id="${headId}">${esc(t(g.titleKey))}</h3><span class="cap-cat-count" aria-hidden="true">${g.rows.length}</span>${capBulkGroup(g.key, g.titleKey, g.rows)}</header><div class="cap-perms" role="list">${g.rows.map(corePermRow).join("")}</div></section>`;
}

/// Whether the last override write failed: after refresh the msg area shows 'Save failed' in red, cleared on consumption.
let corePermSaveFailed = false;

/// System Capabilities tab (docs/permissions.md 'Global policy (hard gate)'): grouped by category,
/// control granularity = permission (matching check_agent's enforcement granularity; capabilities that share a permission are listed together,
/// so subscription-type file.watch / screen.watch cannot be toggled independently of the base permission).
/// focusSel: the select selector whose focus is restored after redraw; passed only on the keyboard/change path, not on initial load or manual refresh.
async function refreshCorePerms(focusSel: string | null = null, allowEnter = false): Promise<void> {
  const box = document.getElementById("core-perm-list");
  if (!box) return;
  // The enter animation is allowed only on the 'tab-switch render'. .fresh-tab lingers in #tab-body across refreshes,
  // and this function rebuilds the cards every time — without explicitly turning it off, clicking a tier replays the blur-and-rise across the whole page,
  // and the row just changed disappears for over a second (measured: at 120ms later cards already have opacity 0 and filter blur(4px)).
  box.classList.toggle("cap-no-enter", !allowEnter);
  // Only on first load (container still empty) lay out the skeleton + aria-busy; later refreshes don't touch the DOM, per-row toggles don't flash the skeleton
  if (box.childElementCount === 0) {
    box.setAttribute("aria-busy", "true");
    box.innerHTML = listSkeleton(3);
  }
  let list: CorePermPolicy[] = [];
  let failed = false;
  try {
    list = await invoke<CorePermPolicy[]>("core_permission_list");
  } catch {
    failed = true;
  }
  box.removeAttribute("aria-busy");
  const msg = document.getElementById("core-perm-msg");
  if (failed) {
    if (msg) {
      msg.textContent = "";
      msg.classList.remove("cap-msg-failed");
    }
    // Fetch failure != no permissions: show a retryable error block, don't pass empty-state text off as it
    box.innerHTML = listError("corePermLoadFailed", "core-perm-retry");
    document.getElementById("core-perm-retry")?.addEventListener("click", () => void refreshCorePerms());
    return;
  }
  if (msg) {
    // If the last write failed, show 'Save failed' in red first; the next successful refresh restores the count
    msg.textContent = corePermSaveFailed ? t("corePermSaveFailed") : `${list.filter((e) => e.override !== null).length}/${list.length}`;
    msg.classList.toggle("cap-msg-failed", corePermSaveFailed);
    corePermSaveFailed = false;
  }
  if (list.length === 0) {
    box.innerHTML = `<p class="empty">${esc(t("corePermNone"))}</p>`;
    return;
  }
  const byPerm = new Map<string, CorePermPolicy>();
  list.forEach((e) => byPerm.set(e.permission, e));
  const groups: Array<{ key: string; titleKey: I18nKey; rows: CorePermPolicy[] }> = [];
  const claimed = new Set<string>();
  for (const cat of CORE_PERM_CATEGORIES) {
    const rows = cat.permissions
      .map((p) => byPerm.get(p))
      .filter((e): e is CorePermPolicy => !!e);
    if (rows.length === 0) continue;
    rows.forEach((e) => claimed.add(e.permission));
    groups.push({ key: cat.key, titleKey: cat.titleKey, rows });
  }
  const rest = list.filter((e) => !claimed.has(e.permission));
  if (rest.length > 0) groups.push({ key: "other", titleKey: "capCatOther", rows: rest });
  box.innerHTML = groups.map(capCategory).join("");
  // Persist a single permission: '' = clear override (follow default), otherwise write the override tier; failures set a flag, shown in the msg area after refresh
  const applyPerm = async (permission: string, value: string): Promise<void> => {
    try {
      if (value === "") {
        await invoke("core_permission_reset", { permission });
      } else {
        await invoke("core_permission_set", { permission, decision: value });
      }
    } catch {
      /* High-risk rejected granted etc.: the action must not be silent */
      corePermSaveFailed = true;
    }
  };
  box.querySelectorAll<HTMLSelectElement>("select[data-cap-bulk]").forEach((s) => {
    s.addEventListener("change", async () => {
      const key = s.dataset.capBulk ?? "";
      const value = s.value;
      if (value === "__mixed__") return;
      const rows = groups.find((g) => g.key === key)?.rows ?? [];
      for (const row of rows) {
        if (row.highRisk && value === "granted") continue;
        await applyPerm(row.permission, value);
      }
      // After redraw return focus to that category's bulk control: the native select fires change on arrow keys, and losing focus breaks the keyboard flow
      await refreshCorePerms(`select[data-cap-bulk="${key}"]`);
    });
  });
  box.querySelectorAll<HTMLSelectElement>("select[data-cap-perm]").forEach((s) => {
    s.addEventListener("change", async () => {
      const permission = s.dataset.capPerm ?? "";
      await applyPerm(permission, s.value);
      await refreshCorePerms(`select[data-cap-perm="${permission}"]`);
    });
  });
  if (focusSel) box.querySelector<HTMLSelectElement>(focusSel)?.focus();
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

interface PluginStatus {
  id: string;
  name: string;
  description?: string;
  author?: string;
  homepage?: string;
  license?: string;
  version: string;
  status: string;
  capabilities: string[];
  permissions: string[];
  path?: string;
  autoReload: boolean;
  /// Phase 37 — probe status ('passed' / 'failed' / 'pending' / 'skipped'); undefined if never run.
  probeStatus?: string;
  /// Phase 37 — timestamp when probe ran (epoch seconds).
  probeAt?: number;
  /// Phase 38 — plugin self-update channel ('stable' / 'beta' / 'dev'); undefined if unset.
  channel?: string;
  /// S5 — whether a sandbox block is declared (declared != execution enabled).
  sandboxDeclared?: boolean;
  /// Phase 40 — watchdog heartbeat interval (seconds). 0 = ping off, falling back to the 500ms is_alive check.
  healthHeartbeatSec?: number;
  /// Phase 40 — failure retry limit. Exceeding it -> switch to status='error' + emit plugin.watchdog_disabled.
  /// 0 = disable the watchdog.
  healthMaxRetries?: number;
  /// Phase 40 — watchdog master switch. false = the current watchdog exits immediately.
  healthEnabled?: boolean;
  /// F5 — unmet plugin dependencies (missing install or version too low); a non-empty list shows a warning on the plugin row.
  missingDependencies?: { id: string; requirement: string }[];
  /// M4 — revocation hit: non-empty when the publisher key is in the registry revokedKeys; the plugin row shows a disabled banner.
  revokedKey?: string;
  /// M4 — revocation hit time (epoch seconds).
  revokedAt?: number;
}

interface PluginUpdateInfo {
  id: string;
  currentVersion: string;
  latestVersion: string;
  downloadUrl: string;
  sha256: string;
  /// Phase 38 — the channel the update came from, used to color the update badge.
  channel?: string;
}

/// F7 — update preview response (preview_update command): unified rendering + install after confirmation without a second download.
interface UpdatePreviewResponse {
  preview: PluginPreview;
  archivePath: string;
  currentVersion: string;
  publisherChange?: { from?: string | null; to?: string | null } | null;
}

const CHANNEL_OPTIONS: Array<{ key: string; i18nKey: string }> = [
  { key: "stable", i18nKey: "channelStable" },
  { key: "beta", i18nKey: "channelBeta" },
  { key: "dev", i18nKey: "channelDev" },
];

function channelBadgeHtml(channel: string | undefined): string {
  if (!channel) return "";
  return `<span class="channel-badge channel-${esc(channel)}" title="${esc(t("channelBadgeTitle"))}">${esc(channel)}</span>`;
}

let pluginUpdates: Record<string, PluginUpdateInfo> = {};

interface PermissionEntry {
  permission: string;
  decision: string;
  default: string;
  high_risk: boolean;
  /// permission-domains §4.3: declared derived permissions are once-only; the UI offers no granted option.
  declared: boolean;
}

interface PluginPermissions {
  plugin_id: string;
  name: string;
  status: string;
  permissions: PermissionEntry[];
}

interface AuditEntry {
  id: string;
  kind: string;
  plugin_id: string;
  permission: string;
  decision: string;
  reason: string;
  timestamp: number;
}

/// i2 §14 Activity Timeline entry (the backend timeline_events are already allowlist-filtered).
/// Text assembly happens in this layer; presentation does not go into Rust.
interface TimelineEntry {
  id: string;
  timestamp: number;
  kind: string;
  category: string;
  agent: string;
  source: string;
  payload: Record<string, unknown>;
}

const TL_ICONS: Record<string, string> = {
  agent: "🤖",
  capability: "⚡",
  permission: "🔐",
  security: "🛡️",
  plugin: "🧩",
  pet: "🐱",
  system: "⚙️",
  notification: "🔔",
};

/// {k} placeholder substitution; values come from payload fields (toString), defaulting to an empty string.
function tlFmt(tpl: string, p: Record<string, unknown>): string {
  return tpl.replace(/\{(\w+)\}/g, (_, k: string) => {
    const v = p[k];
    return v === undefined || v === null ? "" : String(v);
  });
}

/// kind -> localized text. Kinds not covered fall back to the generic template.
function tlText(e: TimelineEntry): string {
  const p = e.payload;
  const str = (k: string): string => (typeof p[k] === "string" ? (p[k] as string) : "");
  switch (e.kind) {
    case "agent.started": return t("tlAgentStarted");
    case "agent.registered": return t("tlAgentRegistered");
    case "capability.completed":
      return tlFmt(t("tlCapCompleted"), { capability: str("capability"), plugin: str("pluginId"), ms: p.elapsedMs ?? "" });
    case "capability.failed": {
      // payload: attemptedProviders[{pluginId, error}] — take the first failed provider
      const att = Array.isArray(p.attemptedProviders) ? (p.attemptedProviders as { pluginId?: string; error?: string }[]) : [];
      const first = att[0] ?? {};
      return tlFmt(t("tlCapFailed"), { capability: str("capability"), plugin: first.pluginId ?? "", reason: first.error ?? "" });
    }
    case "capability.subscribed":
      return tlFmt(t("tlSubscribed"), { capability: str("capability") });
    case "capability.unsubscribed":
      return tlFmt(t("tlUnsubscribed"), { capability: str("capability"), reason: str("reason") });
    case "permission.granted":
      return tlFmt(t("tlPermGranted"), { permission: str("permission") });
    case "permission.denied":
      return tlFmt(t("tlPermDenied"), { permission: str("permission") });
    case "permission.requested":
      return tlFmt(t("tlPermRequested"), { permission: str("permission") });
    case "auth.rejected":
      return tlFmt(t("tlAuthRejected"), { reason: str("reason") });
    case "plugin.installed":
      return tlFmt(t("tlPluginInstalled"), { plugin: str("pluginId"), version: str("version") });
    case "plugin.uninstalled":
      return tlFmt(t("tlPluginUninstalled"), { plugin: str("pluginId") });
    case "plugin.signature.verified":
      return tlFmt(t("tlPluginSignature"), { plugin: str("id"), key: str("keyId") });
    case "plugin.start.rejected":
      return tlFmt(t("tlPluginStartRejected"), { plugin: str("pluginId"), reason: str("reason") });
    case "plugin.kill_switch.enabled":
      return tlFmt(t("tlPluginKilled"), { reason: str("reason") });
    case "plugin.lifecycle.crashed":
      return tlFmt(t("tlPluginCrashed"), { plugin: str("pluginId"), reason: str("reason") });
    case "pet.say":
      return tlFmt(t("tlPetSay"), { text: str("text") });
    case "pet.state_changed":
      return tlFmt(t("tlPetState"), { state: str("state") });
    case "workspace.switched":
      return tlFmt(t("tlWorkspace"), { profile: str("profile") });
    case "notification.posted":
      return tlFmt(t("tlNotification"), { title: str("title"), body: str("body") });
    default:
      return tlFmt(t("tlGeneric"), { kind: e.kind });
  }
}

/// Timeline entry: collapsed it shows a one-line summary; expand to see category/source/raw payload.
function renderTimelineRow(e: TimelineEntry): string {
  const time = new Date(e.timestamp * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  const who = e.agent || (typeof e.payload.pluginId === "string" ? (e.payload.pluginId as string) : "") || e.source;
  const icon = TL_ICONS[e.category] ?? TL_ICONS.system;
  const fullTs = new Date(e.timestamp * 1000).toISOString().replace("T", " ").slice(0, 19);
  const detail =
    detailKv(t("auditDetailCategory"), e.category) +
    detailKv(t("auditDetailKind"), e.kind) +
    detailKv(t("auditDetailAgent"), e.agent) +
    detailKv(t("auditDetailSource"), e.source) +
    detailKv(t("auditDetailTime"), fullTs) +
    detailPayload(t("auditDetailPayload"), e.payload);
  return `<details class="rec"><summary class="rec-head"><span class="rec-time">${esc(time)}</span><span class="rec-icon">${icon}</span><span class="rec-text"><b>${esc(who)}</b> ${esc(tlText(e))}</span></summary><div class="rec-detail">${detail}</div></details>`;
}

/// View toggle: timeline (default) / perms (legacy permission list). Only touches DOM visibility + button state.
function setAuditView(v: "timeline" | "perms"): void {
  auditView = v;
  document.getElementById("timeline-row")?.toggleAttribute("hidden", v !== "timeline");
  document.getElementById("audit-row")?.toggleAttribute("hidden", v !== "perms");
  const bt = document.getElementById("audit-view-timeline");
  const bp = document.getElementById("audit-view-perms");
  bt?.setAttribute("aria-pressed", String(v === "timeline"));
  bp?.setAttribute("aria-pressed", String(v === "perms"));
  if (v === "timeline") void refreshTimeline();
  else void refreshAudit();
}

async function refreshTimeline(): Promise<void> {
  const box = document.getElementById("timeline-list");
  const msg = document.getElementById("timeline-msg");
  if (!box) return;
  // On first load (container still empty) lay out the skeleton + aria-busy; refresh/retry doesn't touch the DOM and doesn't flash the skeleton
  if (box.childElementCount === 0) {
    box.setAttribute("aria-busy", "true");
    box.innerHTML = listSkeleton(3);
  }
  const agent = (document.getElementById("timeline-agent") as HTMLInputElement | null)?.value.trim() ?? "";
  let entries: TimelineEntry[] = [];
  let failed = false;
  try {
    entries = await invoke<TimelineEntry[]>("timeline_events", { limit: 300, agent: agent || null });
  } catch {
    failed = true;
  }
  box.removeAttribute("aria-busy");
  if (failed) {
    if (msg) msg.textContent = "";
    box.innerHTML = listError("listLoadFailed", "timeline-retry");
    document.getElementById("timeline-retry")?.addEventListener("click", () => void refreshTimeline());
    return;
  }
  if (entries.length === 0) {
    box.innerHTML = `<div class="list-empty">${esc(t("timelineEmpty"))}</div>`;
    if (msg) msg.textContent = "";
    return;
  }
  // Group by day (the backend returns newest-first; within a group keep that order, across groups show nearest to farthest)
  const groups: { day: string; items: TimelineEntry[] }[] = [];
  for (const e of entries) {
    const day = dayLabel(e.timestamp);
    const last = groups[groups.length - 1];
    if (last && last.day === day) last.items.push(e);
    else groups.push({ day, items: [e] });
  }
  box.innerHTML = groups
    .map((g) => `<div class="tl-day">${esc(g.day)}</div>` + g.items.map(renderTimelineRow).join(""))
    .join("");
  if (msg) msg.textContent = `${entries.length} ${t("listItems")}`;
}

// Phase 34 — Event stream record/replay UI.
interface ReplaySession {
  sessionId: string;
  startedAt: number;
  endedAt?: number;
  sizeBytes: number;
  lineCount: number;
}

async function refreshReplay(): Promise<void> {
  const box = document.getElementById("replay-list");
  const msg = document.getElementById("replay-msg");
  if (!box) return;
  if (box.childElementCount === 0) {
    box.setAttribute("aria-busy", "true");
    box.innerHTML = listSkeleton(2);
  }
  let sessions: ReplaySession[] = [];
  let failed = false;
  try {
    sessions = await invoke<ReplaySession[]>("list_replay_sessions");
  } catch {
    failed = true;
  }
  box.removeAttribute("aria-busy");
  if (failed) {
    if (msg) msg.textContent = "";
    box.innerHTML = listError("listLoadFailed", "replay-retry");
    document.getElementById("replay-retry")?.addEventListener("click", () => void refreshReplay());
    return;
  }
  if (sessions.length === 0) {
    box.innerHTML = `<p class="list-empty">${esc(t("replayEmpty"))}</p>`;
    if (msg) msg.textContent = "";
    return;
  }
  const rows = sessions
    .map((s) => {
      const startDate = new Date(s.startedAt * 1000).toLocaleString();
      const dur = s.endedAt ? `${s.endedAt - s.startedAt}s` : "ongoing";
      return `<tr><td><code>${esc(s.sessionId)}</code></td><td>${esc(startDate)}</td><td>${esc(dur)}</td><td>${s.lineCount}</td><td>${esc(formatBytes(s.sizeBytes))}</td><td><input type="text" class="replay-filter" placeholder="${esc(t("replayFilter"))}" aria-label="${esc(t("replayFilter"))}: ${esc(s.sessionId)}" data-sid="${esc(s.sessionId)}" /><button class="btn ghost replay-go" data-sid="${esc(s.sessionId)}">▶</button><button class="btn ghost replay-export" data-sid="${esc(s.sessionId)}">⬇</button></td></tr>`;
    })
    .join("");
  box.innerHTML = `<table class="replay-table"><thead><tr><th>${esc(t("replaySession"))}</th><th>${esc(t("replayStarted"))}</th><th>${esc(t("replayDuration"))}</th><th>${esc(t("replayEvents"))}</th><th>${esc(t("replaySize"))}</th><th>${esc(t("replayActions"))}</th></tr></thead><tbody>${rows}</tbody></table>`;
  if (msg) msg.textContent = `${sessions.length} ${esc(t("replaySessions"))}`;
  box.querySelectorAll<HTMLButtonElement>(".replay-go").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const sid = btn.dataset.sid!;
      const filter = (box.querySelector(`input.replay-filter[data-sid="${CSS.escape(sid)}"]`) as HTMLInputElement)?.value.trim() ?? "";
      const n = await invoke<number>("replay_session_to_stream", { sessionId: sid, filterKind: filter });
      if (msg) msg.textContent = `${t("replayReplayed")} ${n}`;
    });
  });
  box.querySelectorAll<HTMLButtonElement>(".replay-export").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const sid = btn.dataset.sid!;
      const events = await invoke<{ kind: string; timestamp: number; source: string; payload: unknown }[]>("get_replay_events", { sessionId: sid, limit: 50000 });
      const lines = events.map((e) => JSON.stringify({ id: "", type: e.kind, source: e.source, timestamp: e.timestamp, payload: e.payload })).join("\n");
      const blob = new Blob([lines + "\n"], { type: "application/x-ndjson" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `opencapx-replay-${sid}.ndjson`;
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      URL.revokeObjectURL(url);
      if (msg) msg.textContent = t("replayExported");
    });
  });
}

// ─── i2 §15 Automation ───────────────────────────────────────────────────────

interface AutomationRule {
  id: string;
  enabled?: boolean;
  when?: { event?: string; match?: Record<string, unknown> };
  then?: { action?: string; title?: string; body?: string; text?: string };
}

interface RuleSummary {
  id: string;
  enabled: boolean;
  stage: string;
  matcher: string;
  action: string;
  source: string;
}

/// notify uses title+body, say uses text; toggle input visibility by action.
/// In the dialog the input is wrapped in .filter-field: toggling only the input's hidden leaves an empty label, so toggle the wrapper too.
function syncAutomationActionFields(): void {
  const action = (document.getElementById("auto-action") as HTMLSelectElement | null)?.value ?? "notify";
  const notifyOnly = ["auto-title", "auto-body"];
  for (const id of notifyOnly) toggleAutoField(id, action === "notify");
  toggleAutoField("auto-text", action === "say");
}

/// Collapse/expand one dialog field: toggle the hidden state of both the input itself (by id, preserving sync's original semantics) and its label wrapper.
function toggleAutoField(id: string, visible: boolean): void {
  const el = document.getElementById(id);
  el?.toggleAttribute("hidden", !visible);
  el?.closest(".filter-field")?.toggleAttribute("hidden", !visible);
}

/// Rule row: multi-line layout — event / condition / action each on its own line; match is rendered as k = v pairs instead of raw JSON.
/// Enabled state uses two channels: dot color (.dot.done/.idle) + button text (enable/disable), not color alone.
function automationRuleRow(r: AutomationRule): string {
  const on = r.enabled !== false;
  const match = r.when?.match ? Object.entries(r.when.match).map(([k, v]) => `${k} = ${JSON.stringify(v)}`).join(" · ") : "";
  const actionName = r.then?.action === "say" ? t("automationActionSay") : t("automationActionNotify");
  const detail = r.then?.action === "say" ? (r.then.text ?? "") : [r.then?.title, r.then?.body].filter(Boolean).join(" · ");
  return `<div class="sess rule-row"><span class="dot ${on ? "done" : "idle"}"></span><div class="rule-text"><div class="rule-event">${esc(r.when?.event ?? "—")}</div>${match ? `<div class="rule-cond">${esc(match)}</div>` : ""}<div class="rule-action">${esc(actionName)}${detail ? ` · ${esc(detail)}` : ""}</div></div><div class="rule-btns"><button class="btn ghost" data-auto-toggle="${esc(r.id)}" type="button">${esc(on ? t("automationEnabled") : t("automationDisabled"))}</button><button class="btn ghost danger" data-auto-del="${esc(r.id)}" type="button">${esc(t("automationRemove"))}</button></div></div>`;
}

async function refreshAutomation(): Promise<void> {
  const box = document.getElementById("automation-list");
  const msg = document.getElementById("automation-msg");
  if (!box) return;
  if (box.childElementCount === 0) {
    box.setAttribute("aria-busy", "true");
    box.innerHTML = listSkeleton(3);
  }
  let rules: AutomationRule[] = [];
  let failed = false;
  try {
    rules = await invoke<AutomationRule[]>("list_automation_rules");
  } catch {
    failed = true;
  }
  box.removeAttribute("aria-busy");
  if (failed) {
    if (msg) msg.textContent = "";
    box.innerHTML = listError("listLoadFailed", "automation-retry");
    document.getElementById("automation-retry")?.addEventListener("click", () => void refreshAutomation());
    return;
  }
  if (rules.length === 0) {
    box.innerHTML = `<div class="list-empty">${esc(t("automationEmpty"))}</div>`;
    if (msg) msg.textContent = "";
    return;
  }
  box.innerHTML = rules.map(automationRuleRow).join("");
  box.querySelectorAll("button[data-auto-toggle]").forEach((b) => {
    b.addEventListener("click", async () => {
      const id = (b as HTMLElement).dataset.autoToggle ?? "";
      const rule = rules.find((r) => r.id === id);
      await invoke("set_automation_rule_enabled", { id, enabled: !(rule?.enabled !== false) });
      await refreshAutomation();
    });
  });
  box.querySelectorAll("button[data-auto-del]").forEach((b) => {
    b.addEventListener("click", async () => {
      await invoke("remove_automation_rule", { id: (b as HTMLElement).dataset.autoDel ?? "" });
      await refreshAutomation();
    });
  });
  if (msg) msg.textContent = `${rules.length} ${t("listItems")}`;
}

/// Inline message funnel: textContent + error-state class in one pass; empty text also clears the error state,
/// so a later refresh/success receipt doesn't inherit the previous round's red text.
function setRuleMsg(msg: HTMLElement | null, text: string, isErr = false): void {
  if (!msg) return;
  msg.textContent = text;
  msg.classList.toggle("is-err", isErr && text !== "");
}

/// Command rule row: the source layer is key information — global can be toggled here (the toggle is .toggle-switch),
/// project-level is read-only (changing it requires editing files in the repo); read-only rows also show status text + a read-only hint (not relying on dot color alone).
/// Three inline segments: id/source badge -> match -> rewrite; the control column always has exactly one active control.
function ruleSummaryRow(r: RuleSummary): string {
  const editable = r.source === "global";
  const stateLabel = r.enabled ? t("automationEnabled") : t("automationDisabled");
  const control = editable
    ? `<button class="toggle-switch${r.enabled ? " active" : ""}" data-rule-toggle="${escAttr(r.id)}" type="button" role="switch" aria-checked="${r.enabled}" aria-label="${escAttr(r.id)}" title="${escAttr(stateLabel)}"><span class="toggle-slider"></span></button>`
    : `<span class="rules-state">${esc(stateLabel)} · ${esc(t("rulesReadOnly"))}</span>`;
  const sourceCls = r.source === "global" ? " is-global" : "";
  return `<div class="rules-row">
    <span class="rules-dot ${r.enabled ? "on" : "off"}" aria-hidden="true"></span>
    <div class="rules-main">
      <div class="rules-head"><span class="rules-id">${esc(r.id)}</span><span class="rules-source${sourceCls}" title="${escAttr(r.source)}">${esc(r.source)}</span></div>
      <div class="rules-detail">${r.matcher ? `<span class="rules-match">${esc(r.matcher)}</span><span class="rules-arrow" aria-hidden="true">→</span>` : ""}<span class="rules-action">${esc(r.action)}</span></div>
    </div>
    <div class="rules-side">${control}</div>
  </div>`;
}

async function refreshRules(): Promise<void> {
  const box = document.getElementById("rules-list");
  const msg = document.getElementById("rules-msg");
  if (!box) return;
  if (box.childElementCount === 0) {
    box.setAttribute("aria-busy", "true");
    box.innerHTML = listSkeleton(3);
  }
  let rules: RuleSummary[] = [];
  let failed = false;
  try {
    rules = await invoke<RuleSummary[]>("list_rules");
  } catch {
    failed = true;
  }
  box.removeAttribute("aria-busy");
  if (failed) {
    setRuleMsg(msg, "");
    box.innerHTML = listError("listLoadFailed", "rules-retry");
    document.getElementById("rules-retry")?.addEventListener("click", () => void refreshRules());
    return;
  }
  if (rules.length === 0) {
    box.innerHTML = `<div class="list-empty">${esc(t("rulesEmpty"))}</div>`;
    setRuleMsg(msg, "");
  } else {
    box.innerHTML = rules.map(ruleSummaryRow).join("");
    box.querySelectorAll("button[data-rule-toggle]").forEach((b) => {
      b.addEventListener("click", async () => {
        const id = (b as HTMLElement).dataset.ruleToggle ?? "";
        const rule = rules.find((r) => r.id === id);
        await invoke("set_rule_enabled", { id, enabled: !(rule?.enabled ?? true) });
        await refreshRules();
      });
    });
    setRuleMsg(msg, `${rules.length} ${t("listItems")}`);
  }
  await refreshTrustedProjects();
}

async function refreshTrustedProjects(): Promise<void> {
  const box = document.getElementById("rules-trust-list");
  if (!box) return;
  let paths: string[] = [];
  try {
    paths = await invoke<string[]>("list_trusted_projects");
  } catch {
    paths = [];
  }
  if (paths.length === 0) {
    box.innerHTML = `<div class="list-empty">${esc(t("rulesTrustEmpty"))}</div>`;
    return;
  }
  box.innerHTML = paths
    .map(
      (p) =>
        `<div class="rules-trust-row"><span class="rules-trust-path" title="${escAttr(p)}">${esc(p)}</span><button class="btn ghost danger" data-untrust="${escAttr(p)}" type="button">${esc(t("rulesUntrust"))}</button></div>`,
    )
    .join("");
  box.querySelectorAll("button[data-untrust]").forEach((b) => {
    b.addEventListener("click", async () => {
      await invoke("untrust_project", { path: (b as HTMLElement).dataset.untrust ?? "" });
      await refreshRules();
    });
  });
}

async function addTrustedProject(): Promise<void> {
  const input = document.getElementById("rules-trust-path") as HTMLInputElement | null;
  const msg = document.getElementById("rules-trust-msg");
  const path = input?.value.trim() ?? "";
  if (!path) {
    setRuleMsg(msg, t("rulesTrustNeedPath"), true);
    return;
  }
  try {
    await invoke("trust_project", { path });
    if (input) input.value = "";
    setRuleMsg(msg, t("rulesTrustAdded"));
  } catch (e) {
    setRuleMsg(msg, String(e), true);
  }
  await refreshRules();
}

/// Submit the inline form: the frontend only assembles fields; validation and writing are left to Rust's add_rule
/// (which re-runs RulesFile validation; invalid fields such as shell are rejected here).
async function submitRuleForm(): Promise<void> {
  const msg = document.getElementById("rule-msg");
  const fail = (text: string): void => setRuleMsg(msg, text, true);
  const built = buildRuleFromForm(fail);
  if (!built) return;
  try {
    await invoke("add_rule", { rule: built });
  } catch (e) {
    fail(`${t("rulesAddFailed")} ${String(e)}`);
    return;
  }
  setRuleMsg(msg, t("rulesAdded"));
  clearRuleForm();
  await refreshRules();
}

function clearRuleForm(): void {
  for (const id of [
    "rule-id",
    "rule-match-value",
    "rule-prepend",
    "rule-from",
    "rule-to",
    "rule-env-k",
    "rule-env-v",
  ]) {
    const el = document.getElementById(id) as HTMLInputElement | null;
    if (el) el.value = "";
  }
}

/// Toggle input visibility by the chosen transform (toggling only the input leaves an empty label, so toggle the wrapper too).
function syncRuleXformFields(): void {
  const kind = (document.getElementById("rule-xform-kind") as HTMLSelectElement | null)?.value ?? "prepend";
  const show = (id: string, on: boolean): void => {
    const el = document.getElementById(id);
    if (el) (el as HTMLElement).hidden = !on;
  };
  show("rule-f-prepend", kind === "prepend");
  show("rule-f-from", kind === "replace_binary");
  show("rule-f-to", kind === "replace_binary");
  show("rule-f-envk", kind === "env");
  show("rule-f-envv", kind === "env");
}

/// Assemble the rule JSON; when fields are incomplete, report an error via fail and return null.
function buildRuleFromForm(fail: (text: string) => void): Record<string, unknown> | null {
  const val = (id: string): string =>
    (document.getElementById(id) as HTMLInputElement | null)?.value.trim() ?? "";
  const matchKind = (document.getElementById("rule-match-kind") as HTMLSelectElement | null)?.value ?? "prefix";
  const matchValue = val("rule-match-value");
  if (!matchValue) {
    fail(t("rulesNeedMatch"));
    return null;
  }
  const xformKind = (document.getElementById("rule-xform-kind") as HTMLSelectElement | null)?.value ?? "prepend";
  const then: Record<string, unknown> = { action: "rewrite" };
  if (xformKind === "prepend") {
    const wrapper = val("rule-prepend");
    if (!wrapper) {
      fail(t("rulesNeedTransform"));
      return null;
    }
    then.prepend = wrapper;
  } else if (xformKind === "replace_binary") {
    const from = val("rule-from");
    const to = val("rule-to");
    if (!from || !to) {
      fail(t("rulesNeedTransform"));
      return null;
    }
    then.replace_binary = { from, to };
  } else {
    const key = val("rule-env-k");
    const value = val("rule-env-v");
    if (!key || !value) {
      fail(t("rulesNeedTransform"));
      return null;
    }
    then.env = { [key]: value };
  }
  const rule: Record<string, unknown> = {
    enabled: true,
    when: { stage: "tool_pre", command: { [matchKind]: matchValue } },
    then,
  };
  const id = val("rule-id");
  if (id) rule.id = id;
  return rule;
}

/// Add-rule dialog: reuses the .install-dialog look and the existing overlay construction (click backdrop to close, Escape to close).
/// Validation/write errors go only to #auto-dlg-msg inside the dialog, not to the tab's #automation-msg; on success clear fields, close the dialog, refresh the list.
/// Focus: enters at #auto-event, returns to #auto-add.
function openAutomationDialog(): void {
  const overlay = document.createElement("div");
  overlay.id = "auto-dialog-overlay";
  overlay.style.cssText =
    "position:fixed;inset:0;background:rgba(0,0,0,0.55);z-index:9999;display:flex;align-items:center;justify-content:center;backdrop-filter:blur(4px);";
  overlay.innerHTML = `
    <div class="install-dialog auto-dialog" role="dialog" aria-label="${esc(t("automationAdd"))}">
      <h2>${esc(t("automationAdd"))}</h2>
      <div class="auto-form">
        <label class="filter-field"><span class="filter-label">${esc(t("automationEventLabel"))}</span><input type="text" id="auto-event" placeholder="${esc(t("automationEventPlaceholder"))}" aria-label="${esc(t("automationEventLabel"))}" /></label>
        <label class="filter-field"><span class="filter-label">${esc(t("automationMatchLabel"))}</span><input type="text" id="auto-match" placeholder="${esc(t("automationMatchPlaceholder"))}" aria-label="${esc(t("automationMatchLabel"))}" /></label>
        <label class="filter-field"><span class="filter-label">${esc(t("automationActionLabel"))}</span><select id="auto-action" aria-label="${esc(t("automationActionLabel"))}"><option value="notify">${esc(t("automationActionNotify"))}</option><option value="say">${esc(t("automationActionSay"))}</option></select></label>
        <label class="filter-field"><span class="filter-label">${esc(t("automationTitlePlaceholder"))}</span><input type="text" id="auto-title" aria-label="${esc(t("automationTitlePlaceholder"))}" /></label>
        <label class="filter-field"><span class="filter-label">${esc(t("automationBodyPlaceholder"))}</span><input type="text" id="auto-body" aria-label="${esc(t("automationBodyPlaceholder"))}" /></label>
        <label class="filter-field"><span class="filter-label">${esc(t("automationSayPlaceholder"))}</span><input type="text" id="auto-text" aria-label="${esc(t("automationSayPlaceholder"))}" hidden /></label>
      </div>
      <span class="auto-msg" id="auto-dlg-msg"></span>
      <div class="install-actions">
        <button class="btn ghost" id="auto-cancel" type="button">${esc(t("installPreviewCancel"))}</button>
        <button class="btn primary" id="auto-save" type="button">${esc(t("automationAdd"))}</button>
      </div>
    </div>`;
  document.body.appendChild(overlay);
  const close = (): void => {
    document.removeEventListener("keydown", onKey, true);
    overlay.remove();
    document.getElementById("auto-add")?.focus();
  };
  function onKey(ev: KeyboardEvent): void {
    if (ev.key === "Escape") close();
  }
  document.addEventListener("keydown", onKey, true);
  overlay.addEventListener("click", (ev) => {
    if (ev.target === overlay) close();
  });
  overlay.querySelector("#auto-cancel")?.addEventListener("click", close);
  overlay.querySelector("#auto-save")?.addEventListener("click", async () => {
    const ok = await onAddAutomationRule((text) => {
      const m = overlay.querySelector("#auto-dlg-msg");
      if (m) m.textContent = text;
    });
    if (ok) close();
  });
  document.getElementById("auto-action")?.addEventListener("change", () => syncAutomationActionFields());
  syncAutomationActionFields();
  document.getElementById("auto-event")?.focus();
}

/// Validate -> write (payload is still { when, then }) -> clear fields. Returns whether it succeeded.
/// errorTo: where error text lands (passed by the dialog); defaults to the tab's #automation-msg (matching old behavior).
async function onAddAutomationRule(errorTo?: (text: string) => void): Promise<boolean> {
  const msg = document.getElementById("automation-msg");
  const fail = (text: string): boolean => {
    if (errorTo) errorTo(text);
    else if (msg) msg.textContent = text;
    return false;
  };
  const val = (id: string) => (document.getElementById(id) as HTMLInputElement | null)?.value.trim() ?? "";
  const event = val("auto-event");
  if (!event) return fail(t("automationNeedEvent"));
  const matchRaw = val("auto-match");
  let when: Record<string, unknown> = { event };
  if (matchRaw) {
    try {
      when = { event, match: JSON.parse(matchRaw) };
    } catch {
      return fail(t("automationBadJson"));
    }
  }
  const action = (document.getElementById("auto-action") as HTMLSelectElement | null)?.value ?? "notify";
  const then = action === "say" ? { action, text: val("auto-text") } : { action, title: val("auto-title"), body: val("auto-body") };
  try {
    await invoke("add_automation_rule", { when, then });
    for (const id of ["auto-event", "auto-match", "auto-title", "auto-body", "auto-text"]) {
      const el = document.getElementById(id) as HTMLInputElement | null;
      if (el) el.value = "";
    }
    if (msg) msg.textContent = t("automationAdded");
    await refreshAutomation();
    return true;
  } catch (e) {
    return fail(`✗ ${String(e)}`);
  }
}

// Phase 35 — read the current filter conditions from the Search/filter UI; an empty value means 'no filter' and returns null.
function readAuditFilter(): {
  kindPrefix: string | null;
  query: string | null;
  sinceTs: number | null;
  untilTs: number | null;
} {
  const q = (document.getElementById("audit-q") as HTMLInputElement | null)?.value.trim() ?? "";
  const prefix = (document.getElementById("audit-prefix") as HTMLInputElement | null)?.value.trim() ?? "";
  const sinceStr = (document.getElementById("audit-since") as HTMLInputElement | null)?.value ?? "";
  const untilStr = (document.getElementById("audit-until") as HTMLInputElement | null)?.value ?? "";
  // Parse datetime-local into unix seconds (Date.parse returns ms)
  const since = sinceStr ? Math.floor(new Date(sinceStr).getTime() / 1000) : 0;
  const until = untilStr ? Math.floor(new Date(untilStr).getTime() / 1000) : 0;
  return {
    kindPrefix: prefix || null,
    query: q || null,
    sinceTs: since > 0 ? since : null,
    untilTs: until > 0 ? until : null,
  };
}

// Whether any filter condition is active
function auditFilterActive(): boolean {
  const f = readAuditFilter();
  return !!(f.kindPrefix || f.query || f.sinceTs || f.untilTs);
}

async function refreshAudit(): Promise<void> {
  const box = document.getElementById("audit-list");
  const msg = document.getElementById("audit-msg");
  if (!box) return;
  if (box.childElementCount === 0) {
    box.setAttribute("aria-busy", "true");
    box.innerHTML = listSkeleton(3);
  }
  const filter = readAuditFilter();
  let entries: AuditEntry[] = [];
  let failed = false;
  try {
    if (auditFilterActive()) {
      entries = await invoke<AuditEntry[]>("search_audit_events", {
        filter: {
          kindPrefix: filter.kindPrefix,
          query: filter.query,
          sinceTs: filter.sinceTs,
          untilTs: filter.untilTs,
          limit: 500,
        },
      });
    } else {
      entries = await invoke<AuditEntry[]>("list_audit", { limit: 200 });
    }
  } catch {
    failed = true;
  }
  box.removeAttribute("aria-busy");
  if (failed) {
    if (msg) msg.textContent = "";
    box.innerHTML = listError("listLoadFailed", "audit-list-retry");
    document.getElementById("audit-list-retry")?.addEventListener("click", () => void refreshAudit());
    return;
  }
  if (entries.length === 0) {
    box.innerHTML = `<div class="list-empty">${esc(t("auditEmpty"))}</div>`;
    if (msg) msg.textContent = "";
    return;
  }
  box.innerHTML = entries.map((e) => renderAuditRow(auditRowFromEntry(e))).join("");
  if (msg) msg.textContent = `${entries.length} ${t("listItems")}`;
}

// Phase 35 — CSV export of the current filtered results (same Blob download pattern as the Phase 34 NDJSON).
// Fields: id, kind, pluginId, permission, decision, reason, timestamp, timestamp_iso
// Escaping: values containing commas/double quotes/newlines -> wrap in double quotes + escape inner double quotes as two.
function csvEscape(v: string): string {
  if (v === "") return "";
  if (/[",\n\r]/.test(v)) return `"${v.replace(/"/g, '""')}"`;
  return v;
}

async function exportAuditCsv(): Promise<void> {
  const msg = document.getElementById("audit-msg");
  try {
    let entries: AuditEntry[];
    const filter = readAuditFilter();
    if (auditFilterActive()) {
      entries = await invoke<AuditEntry[]>("search_audit_events", {
        filter: {
          kindPrefix: filter.kindPrefix,
          query: filter.query,
          sinceTs: filter.sinceTs,
          untilTs: filter.untilTs,
          limit: 5000,
        },
      });
    } else {
      entries = await invoke<AuditEntry[]>("list_audit", { limit: 5000 });
    }
    if (entries.length === 0) {
      if (msg) msg.textContent = t("auditEmpty");
      return;
    }
    const header = ["id", "kind", "pluginId", "permission", "decision", "reason", "timestamp", "timestamp_iso"];
    const rows = entries.map((e) => {
      const ts = new Date(e.timestamp * 1000).toISOString().replace("T", " ").slice(0, 19);
      return [
        e.id,
        e.kind,
        e.plugin_id,
        e.permission,
        e.decision,
        e.reason,
        String(e.timestamp),
        ts,
      ].map(csvEscape).join(",");
    });
    const csv = "﻿" + header.join(",") + "\n" + rows.join("\n") + "\n"; // BOM: make Excel recognize UTF-8
    const blob = new Blob([csv], { type: "text/csv;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    const stamp = new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19);
    a.download = `opencapx-audit-${stamp}.csv`;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
    if (msg) msg.textContent = `${entries.length} ${t("auditExported")}`;
  } catch (err) {
    if (msg) msg.textContent = `✗ ${String(err)}`;
  }
}

interface AuditRowData {
  id: string;
  kind: string;
  pluginId: string;
  permission: string;
  decision: string;
  reason: string;
  timestamp: number;
}

function auditRowFromEvent(e: OpencapxEvent): AuditRowData {
  const p = (e.payload ?? {}) as Record<string, unknown>;
  return {
    id: e.id,
    kind: e.kind,
    pluginId: String(p["pluginId"] ?? "—"),
    permission: String(p["permission"] ?? ""),
    decision: String(p["decision"] ?? e.kind),
    reason: String(p["reason"] ?? ""),
    timestamp: e.timestamp,
  };
}

function auditRowFromEntry(e: AuditEntry): AuditRowData {
  return {
    id: e.id,
    kind: e.kind,
    pluginId: e.plugin_id || "—",
    permission: e.permission,
    decision: e.decision || e.kind,
    reason: e.reason,
    timestamp: e.timestamp,
  };
}

/// Permission decision record: collapsed it shows only 'time · decision · permission · plugin'; expand for the rest.
function renderAuditRow(d: AuditRowData): string {
  const time = new Date(d.timestamp * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  const fullTs = new Date(d.timestamp * 1000).toISOString().replace("T", " ").slice(0, 19);
  const badgeClass = d.decision === "granted" ? " ok" : d.decision === "denied" ? " err" : "";
  const detail =
    detailKv(t("permReason"), d.reason) +
    (d.kind && d.kind !== d.decision ? detailKv(t("auditDetailKind"), d.kind) : "") +
    detailKv(t("auditDetailTime"), fullTs) +
    detailKv(t("auditDetailId"), d.id);
  return `<details class="rec"><summary class="rec-head rec-mono"><span class="rec-time seconds">${esc(time)}</span><span class="rec-badge${badgeClass}">${esc(d.decision)}</span><span class="rec-title">${esc(d.permission)}</span><span class="rec-sub">${esc(d.pluginId)}</span></summary><div class="rec-detail">${detail}</div></details>`;
}

function startAuditStream(): void {
  stopAuditStream();
  const live = document.getElementById("audit-live");
  connectEventStream();
  const setLive = (text: string, cls: string) => {
    if (!live) return;
    live.textContent = text;
    live.className = `audit-live ${cls}`;
  };
  setLive(t("auditConnecting"), "audit-live-pending");
  auditStreamOnEvent = onEvent("*", (ev) => {
    if (!ev.kind.startsWith("permission.")) return;
    const box = document.getElementById("audit-list");
    if (!box) return;
    const empty = box.querySelector(".setting-hint");
    if (empty && empty.textContent === t("auditEmpty")) box.innerHTML = "";
    box.insertAdjacentHTML("afterbegin", renderAuditRow(auditRowFromEvent(ev)));
    setLive(t("auditLive"), "audit-live-on");
  });
  // Simple readiness probe: one fetch to check the /events headers (HEAD won't work — only GET is supported).
  // Failure/reconnect is handled by events.ts itself; this only reflects status.
  setLive(t("auditConnecting"), "audit-live-pending");
}

function stopAuditStream(): void {
  auditStreamOnEvent?.();
  auditStreamOnEvent = null;
}

// ---- Logs tab (plugin.log live stream + history) ----------------------------

interface LogEntry {
  id: string;
  kind: string;
  plugin_id: string;
  level: string;
  source: string;
  message: string;
  timestamp: number;
}

let logsStreamOnEvent: (() => void) | null = null;

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

function readLogFilters(): { query: string; level: string; pluginId: string; sinceTs: number | null } {
  const qEl = document.getElementById("logs-q") as HTMLInputElement | null;
  const lvEl = document.getElementById("logs-level") as HTMLSelectElement | null;
  const piEl = document.getElementById("logs-plugin") as HTMLInputElement | null;
  const query = qEl?.value.trim() ?? "";
  const level = lvEl?.value ?? "";
  const pluginId = piEl?.value.trim() ?? "";
  return { query, level, pluginId, sinceTs: null };
}

function clearLogFilters(): void {
  const qEl = document.getElementById("logs-q") as HTMLInputElement | null;
  const lvEl = document.getElementById("logs-level") as HTMLSelectElement | null;
  const piEl = document.getElementById("logs-plugin") as HTMLInputElement | null;
  if (qEl) qEl.value = "";
  if (lvEl) lvEl.value = "";
  if (piEl) piEl.value = "";
  void refreshLogs(true);
}

function onTailToggle(): void {
  logTailActive = (document.getElementById("logs-tail") as HTMLInputElement | null)?.checked ?? false;
  logTailLastTs = 0;
  if (logTailActive) {
    startLogTailPoll();
  } else {
    stopLogTailPoll();
    void refreshLogs(true);
  }
}

let logTailActive = false;
let logTailLastTs = 0;
let logTailTimer: ReturnType<typeof setInterval> | null = null;

function startLogTailPoll(): void {
  stopLogTailPoll();
  // Immediately pull a round of new events
  void pollLogTail();
  logTailTimer = setInterval(() => void pollLogTail(), 2000);
}

function stopLogTailPoll(): void {
  if (logTailTimer) {
    clearInterval(logTailTimer);
    logTailTimer = null;
  }
}

async function pollLogTail(): Promise<void> {
  if (!logTailActive) return;
  const filters = readLogFilters();
  try {
    const newOnes = await invoke<LogEntry[]>("search_logs", {
      filter: {
        query: filters.query,
        level: filters.level,
        pluginId: filters.pluginId,
        sinceTs: logTailLastTs,
        limit: 100,
      },
    });
    if (newOnes.length > 0) {
      logTailLastTs = Math.max(...newOnes.map((e) => e.timestamp));
      prependLogEntries(newOnes);
    }
  } catch (err) {
    console.warn("log tail poll failed:", err);
  }
}

function prependLogEntries(entries: LogEntry[]): void {
  const box = document.getElementById("logs-list");
  if (!box) return;
  const empty = box.querySelector(".setting-hint");
  if (empty) box.innerHTML = "";
  // Ascending order: old ones inserted first, new ones after
  const html = entries.map((e) => renderLogRow(e)).join("");
  box.insertAdjacentHTML("afterbegin", html);
}

async function refreshLogs(reset: boolean): Promise<void> {
  const box = document.getElementById("logs-list");
  const msg = document.getElementById("logs-msg");
  if (!box) return;
  const filters = readLogFilters();
  try {
    const entries = await invoke<LogEntry[]>("search_logs", {
      filter: {
        query: filters.query,
        level: filters.level,
        pluginId: filters.pluginId,
        sinceTs: null,
        limit: 200,
      },
    });
    if (reset) logTailLastTs = 0;
    if (entries.length > 0) {
      logTailLastTs = Math.max(logTailLastTs, ...entries.map((e) => e.timestamp));
    }
    if (entries.length === 0) {
      box.innerHTML = `<div class="setting-hint">${esc(t("logsEmpty"))}</div>`;
      if (msg) msg.textContent = "";
      return;
    }
    box.innerHTML = entries.map((e) => renderLogRow(e)).join("");
    if (msg)
      msg.textContent = `${entries.length} ${esc(t("logsEntriesShown"))}`;
  } catch (err) {
    box.innerHTML = `<div class="setting-hint">✗ ${esc(String(err))}</div>`;
  }
}

function renderLogRow(e: OpencapxEvent | LogEntry): string {
  const payload = (e as OpencapxEvent).payload as Record<string, unknown> | undefined;
  const pluginId = String(payload?.["pluginId"] ?? (e as LogEntry).plugin_id ?? "—");
  const id = String((e as OpencapxEvent).id ?? (e as LogEntry).id ?? "");
  const level = String(payload?.["level"] ?? (e as LogEntry).level ?? "info").toLowerCase();
  const source = String(payload?.["source"] ?? (e as LogEntry).source ?? "");
  const message = String(payload?.["message"] ?? (e as LogEntry).message ?? "");
  const timestamp = (e as OpencapxEvent).timestamp || (e as LogEntry).timestamp;
  const time = new Date(timestamp * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  const fullTs = new Date(timestamp * 1000).toISOString().replace("T", " ").slice(0, 19);
  const lvlLabel = level === "warn" || level === "warning"
    ? t("logLevelWarn")
    : level === "error" || level === "err"
    ? t("logLevelError")
    : level === "debug"
    ? t("logLevelDebug")
    : t("logLevelInfo");
  const lvlClass = level === "warn" || level === "warning"
    ? "warn"
    : level === "error" || level === "err"
    ? "error"
    : level === "debug"
    ? "debug"
    : "info";
  const srcLabel = source === "stderr" ? t("logSourceStderr") : source === "reverse" ? t("logSourceReverse") : source;
  const detail =
    detailBlock(t("recMessage"), message) +
    detailKv(t("auditDetailSource"), srcLabel) +
    detailKv(t("auditDetailId"), id) +
    detailKv(t("auditDetailTime"), fullTs);
  return `<details class="rec"><summary class="rec-head log-head"><span class="rec-time seconds">${esc(time)}</span><span class="rec-badge log-lvl-${lvlClass}">${esc(lvlLabel)}</span><span class="rec-title">${esc(pluginId)}</span><span class="rec-sub">${esc(message)}</span></summary><div class="rec-detail">${detail}</div></details>`;
}

function startLogsStream(): void {
  stopLogsStream();
  const live = document.getElementById("logs-live");
  connectEventStream();
  const setLive = (text: string, cls: string) => {
    if (!live) return;
    live.textContent = text;
    live.className = `audit-live ${cls}`;
  };
  setLive(t("logsConnecting"), "audit-live-pending");
  logsStreamOnEvent = onEvent("plugin.log", (ev) => {
    const box = document.getElementById("logs-list");
    if (!box) return;
    if (!logEntryMatchesFilters(ev as unknown as LogEntry)) return;
    const empty = box.querySelector(".setting-hint");
    if (empty && empty.textContent === t("logsEmpty")) box.innerHTML = "";
    box.insertAdjacentHTML("afterbegin", renderLogRow(ev));
    setLive(t("logsLive"), "audit-live-on");
  });
}

function logEntryMatchesFilters(e: LogEntry): boolean {
  const filters = readLogFilters();
  if (filters.level && (e.level || "").toLowerCase() !== filters.level.toLowerCase()) return false;
  if (filters.pluginId && e.plugin_id !== filters.pluginId) return false;
  if (filters.query) {
    const q = filters.query.toLowerCase();
    const hay =
      ((e.message || "") + " " + (e.plugin_id || "") + " " + (e.source || "")).toLowerCase();
    if (!hay.includes(q)) return false;
  }
  return true;
}

function stopLogsStream(): void {
  logsStreamOnEvent?.();
  logsStreamOnEvent = null;
}


/// F7 — 'Allow unsigned packages' master switch: read current value + write back (default ON; when OFF, unsigned/unknown-key packages are hard-rejected).
async function initAllowUnsigned(): Promise<void> {
  const box = document.getElementById("allow-unsigned") as HTMLInputElement | null;
  if (!box) return;
  try {
    box.checked = await invoke<boolean>("get_allow_unsigned");
  } catch {
    box.checked = true;
  }
  box.addEventListener("change", async () => {
    try {
      await invoke("set_allow_unsigned", { on: box.checked });
    } catch (e) {
      box.checked = !box.checked;
      const m = document.getElementById("plugin-install-msg");
      if (m) m.textContent = `✗ ${String(e)}`;
    }
  });
}

/// S5b — 'Sandbox execution' toggle (macOS; default OFF = soak first; sandbox-declaring unverified plugins are always forced).
async function initSandboxEnforcement(): Promise<void> {
  const box = document.getElementById("sandbox-enforcement") as HTMLInputElement | null;
  if (!box) return;
  try {
    box.checked = await invoke<boolean>("get_sandbox_enforcement");
  } catch {
    box.checked = false;
  }
  box.addEventListener("change", async () => {
    try {
      await invoke("set_sandbox_enforcement", { on: box.checked });
    } catch (e) {
      box.checked = !box.checked;
      const m = document.getElementById("plugin-install-msg");
      if (m) m.textContent = `✗ ${String(e)}`;
    }
  });
}

/// Shared 'preview -> confirm -> install (including key-change re-confirmation)' sequence: shared by the install path and the local-file update path,
/// guaranteeing the `publisher-key-change-confirm-required` retry logic exists in one place and cannot drift between two.
/// guard: may veto after preview (local-file update uses it to verify the package id against the target plugin id); on refusal it throws a string message.
/// Returns the id after install; cancelling at any step -> null (the caller handles it silently).
async function installPreviewedPlugin(
  path: string,
  opts?: { mode?: "install" | "update"; guard?: (preview: PluginPreview) => string | null },
): Promise<string | null> {
  const preview = await invoke<PluginPreview>("preview_ocplugin", { path });
  const refuse = opts?.guard?.(preview);
  if (refuse) throw refuse;
  const ok = await showInstallPreviewDialog(preview, { mode: opts?.mode ?? "install" });
  if (!ok) return null;
  const softWarn = preview.signature.status === "unsigned" || preview.signature.status === "unknown-key";
  const progress = document.getElementById("plugin-install-msg");
  if (progress) progress.textContent = t("pluginInstalling");
  try {
    return await invoke<string>("install_ocplugin", { path, confirmUnsigned: softWarn });
  } catch (e) {
    const text = String(e);
    if (!text.includes("publisher-key-change-confirm-required")) throw e;
    // F7 — key change = new publisher: after warning confirmation, retry with confirm.
    const go = await showWarningConfirmDialog({
      title: t("pluginRiskConfirmTitle"),
      body: text,
      confirmLabel: t("installPreviewConfirm"),
    });
    if (!go) return null;
    return await invoke<string>("install_ocplugin", {
      path,
      confirmUnsigned: softWarn,
      confirmKeyChange: true,
    });
  }
}

async function pickAndInstallPlugin(): Promise<void> {
  const msg = document.getElementById("plugin-install-msg");
  try {
    const picked = await open({
      multiple: false,
      directory: false,
      filters: [{ name: "OpenCapX Plugin", extensions: ["ocplugin"] }],
    });
    if (!picked) return;
    const path = typeof picked === "string" ? picked : picked;
    // Wow 5 + F7 — preview before installing (tri-state badge + registry verification / official / compatible / diff);
    // warning tiers require an explicit checkbox in the dialog; only after user confirmation is it actually unpacked.
    const id = await installPreviewedPlugin(path);
    if (!id) return;
    if (msg) msg.textContent = `✓ ${id}`;
  } catch (e) {
    if (msg) msg.textContent = `✗ ${String(e)}`;
  }
  await refreshPlugins();
}

interface PermissionPreviewItem {
  name: string;
  highRisk: boolean;
}

interface PluginPreview {
  id: string;
  name: string;
  description?: string;
  version: string;
  type: string;
  capabilities: string[];
  permissions: PermissionPreviewItem[];
  // Wow 6 — signature/integrity status.
  // status ∈ "trusted" | "unsigned" | "tampered" | "bad-signature" | "unknown-key" | "malformed-signature"
  // keyId: signer identifier (returned when trusted / unknown-key)
  signature: { status: string; keyId?: string };
  // F7 — tri-state render facts: registry verification / official flag / core compatibility / permission diff.
  verified?: boolean;
  official?: boolean;
  compat: { ok: boolean; minCoreVersion?: string; current: string };
  permissionDiff?: { added: string[]; removed: string[] };
  /// S5 — whether a sandbox block is declared (used for the forced-execution notice when unsigned).
  sandboxDeclared?: boolean;
}

function signatureBadgeHtml(sig: PluginPreview["signature"]): string {
  const { status, keyId } = sig;
  const cls = `sig-badge sig-${status}`;
  const labelKey = `sigStatus_${status.replace(/-/g, "_")}`;
  let label: string;
  try {
    label = t(labelKey as never);
  } catch {
    label = status;
  }
  const keyTxt = keyId ? ` · ${keyId}` : "";
  return `<span class="${cls}" title="${esc(status)}${esc(keyTxt)}">${esc(label)}${esc(keyTxt)}</span>`;
}

interface UninstallPreview {
  id: string;
  name: string;
  version: string;
  autoReload: boolean;
  configExists: boolean;
  permissionCount: number;
  capabilityCount: number;
  dependents: string[];
}

interface LifecycleEvent {
  id: string;
  kind: string;
  timestamp: number;
  reason: string;
}

/// One row of list_all_traces / list_all_hook_sessions (one line per trace file; pending requests have no endedAt).
/// An empty project string = this trace was written before the project field existed (legacy data); grouped under 'Untagged project'.
interface TraceEntry {
  agentId: string;
  project: string;
  traceId: string;
  startedAt: number;
  endedAt?: number;
  sizeBytes: number;
  lineCount: number;
  /// 'ok' | 'error' | '' ('' = request pending / hook session has no root end).
  status: string;
  /// Last activity time (ms); hook sessions have no root end, so the row header uses it instead of a duration.
  lastTs: number;
}

/** Single /rpc trace row: the `ev` tag distinguishes the three states; camelCase fields are guaranteed by Rust serde rename. */
type RpcTraceLine =
  | { ev: "start"; spanId: string; parentId?: string; name: string; ts: number; attrs: unknown }
  | { ev: "event"; spanId: string; name: string; ts: number; attrs: unknown }
  | { ev: "end"; spanId: string; ts: number; status: "ok" | "error"; error?: string; attrs?: unknown };

interface TraceSummary {
  sessionId: string;
  startedAt: number;
  endedAt?: number;
  sizeBytes: number;
  lineCount: number;
}

interface TraceLine {
  ts: number;
  dir: "in" | "out";
  payload: Record<string, unknown>;
}

interface ProbeCapability {
  capability: string;
  ok: boolean;
  elapsedMs: number;
  error?: string;
}

interface ProbeReport {
  pluginId: string;
  status: "passed" | "failed" | "skipped";
  ranAt: number;
  capabilities: ProbeCapability[];
  summary: string;
}

async function showTraceDialog(pluginId: string, pluginName: string): Promise<void> {
  const overlay = document.createElement("div");
  overlay.style.cssText =
    "position:fixed;inset:0;background:rgba(0,0,0,0.55);z-index:9999;display:flex;align-items:center;justify-content:center;backdrop-filter:blur(4px);";
  overlay.innerHTML = `
    <div class="install-dialog trace-dialog" role="dialog" aria-label="${esc(t("traceTitle"))}">
      <h2>${esc(t("traceTitle"))}</h2>
      <div class="install-head">
        <div class="install-name">${esc(pluginName)}</div>
        <div class="install-id">${esc(pluginId)}</div>
      </div>
      <div class="trace-loading">${esc(t("traceLoading"))}</div>
    </div>`;
  document.body.appendChild(overlay);
  overlay.addEventListener("click", (ev) => {
    if (ev.target === overlay) overlay.remove();
  });
  let sessions: TraceSummary[] = [];
  try {
    sessions = await invoke<TraceSummary[]>("list_plugin_traces", { id: pluginId });
  } catch (err) {
    overlay.innerHTML = `
      <div class="install-dialog trace-dialog" role="dialog" aria-label="${esc(t("traceTitle"))}">
        <h2>${esc(t("traceTitle"))}</h2>
        <p class="muted">${esc(t("traceLoadFailed"))}: ${esc(String((err as Error).message ?? err))}</p>
        <div class="install-actions">
          <button class="btn ghost" id="trace-close" type="button">${esc(t("installPreviewCancel"))}</button>
        </div>
      </div>`;
    overlay.querySelector("#trace-close")?.addEventListener("click", () => overlay.remove());
    return;
  }
  if (sessions.length === 0) {
    overlay.innerHTML = `
      <div class="install-dialog trace-dialog" role="dialog" aria-label="${esc(t("traceTitle"))}">
        <h2>${esc(t("traceTitle"))}</h2>
        <div class="install-head">
          <div class="install-name">${esc(pluginName)}</div>
          <div class="install-id">${esc(pluginId)}</div>
        </div>
        <p class="muted">${esc(t("traceEmpty"))}</p>
        <div class="install-actions">
          <button class="btn ghost" id="trace-close" type="button">${esc(t("installPreviewCancel"))}</button>
        </div>
      </div>`;
    overlay.querySelector("#trace-close")?.addEventListener("click", () => overlay.remove());
    return;
  }
  // Show the session list; clicking switches to details
  const sessionList = sessions
    .map(
      (s) =>
        `<li class="trace-session-row" data-session="${esc(s.sessionId)}"><span class="trace-session-id">${esc(s.sessionId)}</span><span class="trace-session-meta">${s.lineCount} ${esc(t("traceLines"))} · ${formatBytes(s.sizeBytes)}</span></li>`,
    )
    .join("");
  overlay.innerHTML = `
    <div class="install-dialog trace-dialog" role="dialog" aria-label="${esc(t("traceTitle"))}">
      <h2>${esc(t("traceTitle"))}</h2>
      <div class="install-head">
        <div class="install-name">${esc(pluginName)}</div>
        <div class="install-id">${esc(pluginId)} · ${sessions.length} ${esc(t("traceSessions"))}</div>
      </div>
      <ul class="trace-session-list">${sessionList}</ul>
      <div id="trace-detail"></div>
      <div class="install-actions">
        <button class="btn ghost" id="trace-close" type="button">${esc(t("installPreviewCancel"))}</button>
      </div>
    </div>`;
  overlay.querySelector("#trace-close")?.addEventListener("click", () => overlay.remove());
  const detail = overlay.querySelector("#trace-detail") as HTMLElement | null;
  overlay.querySelectorAll<HTMLElement>(".trace-session-row").forEach((row) => {
    row.addEventListener("click", async () => {
      const sid = row.dataset.session ?? "";
      if (!detail) return;
      detail.innerHTML = `<p class="muted">${esc(t("traceLoading"))}</p>`;
      let lines: TraceLine[] = [];
      try {
        lines = await invoke<TraceLine[]>("get_plugin_trace", { id: pluginId, sessionId: sid, limit: 200 });
      } catch (err) {
        detail.innerHTML = `<p class="muted">${esc(t("traceLoadFailed"))}: ${esc(String((err as Error).message ?? err))}</p>`;
        return;
      }
      const body = lines.length
        ? `<ul class="trace-lines">${lines
            .map(
              (l) =>
                `<li><span class="trace-dir trace-dir-${esc(l.dir)}">${l.dir === "in" ? "←" : "→"}</span><span class="trace-ts">${esc(formatLifecycleTime(l.ts))}</span><code class="trace-payload">${esc(JSON.stringify(l.payload))}</code></li>`,
            )
            .join("")}</ul>`
        : `<p class="muted">${esc(t("traceEmpty"))}</p>`;
      detail.innerHTML = `<div class="trace-detail-head">${esc(sid)} · ${lines.length} ${esc(t("traceLines"))}</div>${body}`;
    });
  });
}

/** Span tree row rendering: group the tree by parentId, inline the event rows, indent by depth. */
function renderRpcTraceTree(lines: RpcTraceLine[]): string {
  // get_rpc_trace returns in reverse order -> reverse back to ascending time order to build the tree
  const ordered = [...lines].reverse();
  const starts = new Map<string, Extract<RpcTraceLine, { ev: "start" }>>();
  const ends = new Map<string, Extract<RpcTraceLine, { ev: "end" }>>();
  const events = new Map<string, Extract<RpcTraceLine, { ev: "event" }>[]>();
  for (const l of ordered) {
    if (l.ev === "start") starts.set(l.spanId, l);
    else if (l.ev === "end") ends.set(l.spanId, l);
    else {
      const arr = events.get(l.spanId) ?? [];
      arr.push(l);
      events.set(l.spanId, arr);
    }
  }
  const children = new Map<string, string[]>();
  for (const [id, s] of starts) {
    const key = s.parentId ?? "";
    const arr = children.get(key) ?? [];
    arr.push(id);
    children.set(key, arr);
  }
  const rows: string[] = [];
  // Don't render empty payloads: {} / [] / empty strings (including whitespace-only) are noise in the tree (measured on capability.things.list with {}),
  // rendering them only stretches the row and wastes attention; everything else still outputs full JSON.
  const fmtAttrs = (a: unknown): string => {
    const s = JSON.stringify(a);
    if (s === undefined || s === "{}" || s === "[]" || (typeof a === "string" && a.trim() === "")) return "";
    return ` <code class="trace-payload">${esc(s)}</code>`;
  };
  const walk = (id: string, depth: number): void => {
    const s = starts.get(id);
    if (!s) return;
    const e = ends.get(id);
    const dur = e ? `${e.ts - s.ts}ms` : "…";
    const err = e?.error ? ` <span class="rpc-span-err">${esc(e.error)}</span>` : "";
    rows.push(
      `<li class="rpc-span rpc-span-${e ? e.status : "unset"}" style="margin-left:${depth * 16}px">` +
        `<span class="rpc-span-name">${esc(s.name)}</span><span class="rpc-span-dur">${esc(dur)}</span>${err}` +
        `${fmtAttrs(s.attrs)}</li>`,
    );
    for (const ev of events.get(id) ?? []) {
      rows.push(
        `<li class="rpc-event" style="margin-left:${(depth + 1) * 16}px">· ${esc(ev.name)}${fmtAttrs(ev.attrs)}</li>`,
      );
    }
    for (const c of children.get(id) ?? []) walk(c, depth + 1);
  };
  for (const rootId of children.get("") ?? []) walk(rootId, 0);
  return rows.join("");
}

/// Project grouping: rpc / hook entries with the same project are grouped together; lastTs = the group's most recent activity time (ms).
interface RpcProjectGroup {
  key: string;
  traces: TraceEntry[];
  hooks: TraceEntry[];
  lastTs: number;
}

/// Return value of export_project_chains (Rust serde camelCase). The frontend only reads counts/directory/failure count,
/// it does not parse the chains content — file copying and index.json are all done by the backend.
interface ExportReport {
  dir: string;
  project: string;
  exported: number;
  overwritten: number;
  manifestPath: string;
  chains: { traceId: string; file: string; agentId: string; startedAt: number; endedAt: number | null; lineCount: number; sizeBytes: number; status: string }[];
  failures: { traceId: string; reason: string }[];
}

/// The two full lists from the last successful fetch: exporting all projects reuses the same grouping from groupRpcProjects
/// (including the '' untagged project), without rescanning the DOM or creating another grouping logic.
let rpcLastTraces: TraceEntry[] = [];
let rpcLastHooks: TraceEntry[] = [];

/// Concurrency guard for export-all-projects: any re-trigger while running (including while the directory picker is open) is silently ignored.
let rpcExportAllProjectsBusy = false;

/// Request chains tab: grouped by project, all expanded inline; the two full lists arrive at once and project bodies land with the render (zero requests to expand),
/// only each row's details (trace tree / hook events) are lazy-loaded on first expand.
/// allowEnter: true only on the 'tab-switch render' (plays the enter animation); refresh/retry don't pass it,
/// and .rpc-no-enter explicitly turns it off — .fresh-tab lingers in #tab-body across refreshes, and without turning it off it replays on every refresh.
async function refreshRpcTraces(allowEnter = false): Promise<void> {
  const box = document.getElementById("rpc-trace-list");
  const msg = document.getElementById("rpc-trace-msg");
  if (!box) return;
  box.classList.toggle("rpc-no-enter", !allowEnter);
  if (box.childElementCount === 0) {
    box.setAttribute("aria-busy", "true");
    box.innerHTML = listSkeleton(3);
  }
  // Remember expanded projects and restore them after a refresh (same details[open] approach as refreshIdAgents)
  const openProjects = new Set(
    Array.from(box.querySelectorAll<HTMLDetailsElement>("details[data-rpc-project][open]")).map(
      (d) => d.dataset.rpcProject ?? "",
    ),
  );
  let traces: TraceEntry[] = [];
  let hooks: TraceEntry[] = [];
  let failed = false;
  try {
    [traces, hooks] = await Promise.all([invoke<TraceEntry[]>("list_all_traces"), invoke<TraceEntry[]>("list_all_hook_sessions")]);
  } catch {
    failed = true;
  }
  box.removeAttribute("aria-busy");
  rpcLastTraces = traces;
  rpcLastHooks = hooks;
  if (failed) {
    if (msg) msg.textContent = "";
    box.innerHTML = listError("listLoadFailed", "rpc-trace-retry");
    document.getElementById("rpc-trace-retry")?.addEventListener("click", () => void refreshRpcTraces());
    return;
  }
  if (traces.length === 0 && hooks.length === 0) {
    box.innerHTML = `<div class="list-empty">${esc(t("rpcTraceEmpty"))}</div>`;
    if (msg) msg.textContent = "";
    return;
  }
  const groups = groupRpcProjects(traces, hooks);
  box.innerHTML = `<div class="settings-list">${groups.map((g) => rpcProjectGroup(g, openProjects.has(g.key))).join("")}</div>`;
  wireRpcRows(box);
  wireRpcExportAll(box);
  if (msg) {
    const failedCount = traces.filter((e) => e.status === "error").length;
    msg.textContent = `${traces.length + hooks.length} ${t("listItems")}${failedCount > 0 ? ` · ${failedCount} ${t("rpcTraceFailed")}` : ""}`;
  }
}

/// The two full lists -> grouped by project; most recent activity first, the untagged project ('') always last.
function groupRpcProjects(traces: TraceEntry[], hooks: TraceEntry[]): RpcProjectGroup[] {
  const byKey = new Map<string, RpcProjectGroup>();
  const put = (e: TraceEntry, kind: "rpc" | "hook"): void => {
    let g = byKey.get(e.project);
    if (!g) {
      g = { key: e.project, traces: [], hooks: [], lastTs: 0 };
      byKey.set(e.project, g);
    }
    (kind === "rpc" ? g.traces : g.hooks).push(e);
    g.lastTs = Math.max(g.lastTs, e.endedAt ?? e.startedAt);
  };
  for (const e of traces) put(e, "rpc");
  for (const e of hooks) put(e, "hook");
  return [...byKey.values()].sort((a, b) => {
    if (a.key === "") return 1;
    if (b.key === "") return -1;
    return b.lastTs - a.lastTs;
  });
}

/// Project group row: title + two counts + last activity time (ms -> s then formatted); both body sections render with the list in one pass.
/// When the group has rpc entries with errors, append the failure count to the subtitle — so failures are visible even while collapsed.
function rpcProjectGroup(g: RpcProjectGroup, open: boolean): string {
  const rpcBody = g.traces.length
    ? `<div class="settings-list">${g.traces.map(rpcTraceRow).join("")}</div>`
    : `<p class="list-empty">${esc(t("rpcTraceEmpty"))}</p>`;
  const hookBody = g.hooks.length
    ? `<div class="settings-list">${g.hooks.map(rpcHookRow).join("")}</div>`
    : `<p class="list-empty">${esc(t("traceEmpty"))}</p>`;
  const title = g.key === "" ? t("rpcTraceNoProject") : g.key;
  const failed = g.traces.filter((e) => e.status === "error").length;
  const failSuffix = failed > 0 ? ` · ${failed} ${esc(t("rpcTraceFailed"))}` : "";
  return `<details class="rec" data-rpc-project="${esc(g.key)}"${open ? " open" : ""}><summary class="rec-head"><span class="rec-title">${esc(title)}</span><span class="rec-sub">${g.traces.length} ${esc(t("rpcTraceSectionRpc"))} · ${g.hooks.length} ${esc(t("rpcHookSessions"))}${failSuffix}</span><span class="rec-time seconds">${esc(formatLifecycleTime(Math.floor(g.lastTs / 1000)))}</span><button class="btn ghost rpc-export-all" type="button" data-rpc-export-all="${esc(g.key)}">${esc(t("rpcExportAll"))}</button></summary><div class="rec-detail"><p class="settings-group-title">${esc(t("rpcTraceSectionRpc"))}</p>${rpcBody}<p class="settings-group-title">${esc(t("rpcHookSessions"))}</p>${hookBody}</div></details>`;
}

/// Lazy-load wiring for a row's first expand; re-opening is guarded by the row's own dataset.loaded, so it doesn't refetch.
function wireRpcRows(box: HTMLElement): void {
  box.querySelectorAll<HTMLDetailsElement>("details[data-rpc-trace]").forEach((d) => {
    d.addEventListener("toggle", () => {
      if (d.open) void openRpcTraceDetail(d);
    });
  });
  box.querySelectorAll<HTMLDetailsElement>("details[data-hook-trace]").forEach((d) => {
    d.addEventListener("toggle", () => {
      if (d.open) void openHookTraceDetail(d);
    });
  });
}

/// Wiring for the 'Export all' in the project card header: the button is inside <summary>, so the default behavior must be intercepted,
/// otherwise clicking the button also toggles the card. The project is read from data-rpc-export-all, not matched by text.
function wireRpcExportAll(box: HTMLElement): void {
  box.querySelectorAll<HTMLButtonElement>("button[data-rpc-export-all]").forEach((btn) => {
    btn.addEventListener("click", (ev) => {
      ev.preventDefault(); // intercept <summary>'s default toggle
      ev.stopPropagation(); // intercept bubbling to the card handler
      void runRpcExportAll(btn);
    });
  });
}

/// Export execution: cancel = silent; while running disable and swap the text, restore in finally (so errors don't leave it stuck disabled);
/// result goes into the tab's existing #rpc-trace-msg (hint style on success, the existing .cfg-err red text on failure).
async function runRpcExportAll(btn: HTMLButtonElement): Promise<void> {
  let picked: string | null = null;
  try {
    picked = await open({ directory: true, multiple: false });
  } catch {
    /* Dialog unavailable: treat as cancel, silently */
  }
  if (!picked) return;
  const msgAtPick = document.getElementById("rpc-trace-msg");
  const label = btn.textContent ?? "";
  const project = btn.dataset.rpcExportAll ?? "";
  btn.disabled = true;
  btn.textContent = t("rpcExportAllBusy");
  try {
    const r = await invoke<ExportReport>("export_project_chains", { dir: picked, project });
    const msg = rpcMsgNode(msgAtPick);
    if (msg) {
      msg.classList.remove("cfg-err");
      const overwritten = r.overwritten > 0 ? ` · ${t("rpcExportAllOverwritten").replace("{count}", String(r.overwritten))}` : "";
      const failures = r.failures.length > 0 ? ` · ${t("rpcExportAllFailures").replace("{count}", String(r.failures.length))}` : "";
      msg.textContent = t("rpcExportAllDone").replace("{count}", String(r.exported)).replace("{dir}", r.dir) + overwritten + failures;
    }
  } catch (err) {
    const msg = rpcMsgNode(msgAtPick);
    if (msg) {
      const code = typeof err === "string" ? err : String((err as Error).message ?? err);
      msg.textContent = rpcExportErrorText(code);
      msg.classList.add("cfg-err");
    }
  } finally {
    btn.disabled = false;
    btn.textContent = label;
  }
}

/// The message node may be replaced by a tab re-render: prefer the current DOM one, fall back to the captured one (same double-safety as the button).
function rpcMsgNode(captured: HTMLElement | null): HTMLElement | null {
  return (document.getElementById("rpc-trace-msg") ?? captured) as HTMLElement | null;
}

/// Failure code -> text: three known codes are localized; unknown strings pass through as-is (a generic message would swallow the real reason).
function rpcExportErrorText(code: string): string {
  if (code === "no_chains") return t("rpcExportNoChains");
  if (code === "dir_unwritable") return t("rpcExportDirUnwritable");
  if (code.startsWith("io: ")) return `${t("rpcExportIoFailed")}: ${code.slice(4)}`;
  return code;
}

/// Export all projects: pick a directory once, then export project by project in order (a single project's failure doesn't stop the rest).
/// The project list comes from the same grouping (groupRpcProjects, including the '' untagged project), no DOM scanning and no separate logic;
/// concurrency guard + restore the current DOM button by id in finally — re-renders don't leave a stuck disabled state.
async function runRpcExportAllProjects(btn: HTMLButtonElement): Promise<void> {
  if (rpcExportAllProjectsBusy) return;
  rpcExportAllProjectsBusy = true;
  try {
    let picked: string | null = null;
    try {
      picked = await open({ directory: true, multiple: false });
    } catch {
      /* Dialog unavailable: treat as cancel, silently */
    }
    if (!picked) return;
    const msgAtPick = document.getElementById("rpc-trace-msg");
    btn.disabled = true;
    btn.textContent = t("rpcExportAllBusy");
    let exported = 0;
    let overwritten = 0;
    let failures = 0;
    let okProjects = 0;
    let lastError = "";
    const projects = groupRpcProjects(rpcLastTraces, rpcLastHooks).map((g) => g.key);
    for (const project of projects) {
      try {
        const r = await invoke<ExportReport>("export_project_chains", { dir: picked, project });
        exported += r.exported;
        overwritten += r.overwritten;
        failures += r.failures.length;
        okProjects += 1;
      } catch (err) {
        const code = typeof err === "string" ? err : String((err as Error).message ?? err);
        // no_chains = this project has no exportable chains: count as skipped, not failed
        if (code !== "no_chains") lastError = code;
      }
    }
    const msg = rpcMsgNode(msgAtPick);
    if (msg) {
      if (okProjects === 0 && lastError) {
        msg.textContent = rpcExportErrorText(lastError);
        msg.classList.add("cfg-err");
      } else if (okProjects === 0) {
        // Every project was skipped (or there are no projects): report the no-chains text, not an error
        msg.textContent = t("rpcExportNoChains");
        msg.classList.remove("cfg-err");
      } else {
        const overwrittenText = overwritten > 0 ? ` · ${t("rpcExportAllOverwritten").replace("{count}", String(overwritten))}` : "";
        const failuresText = failures > 0 ? ` · ${t("rpcExportAllFailures").replace("{count}", String(failures))}` : "";
        msg.textContent =
          t("rpcExportAllProjectsDone")
            .replace("{projects}", String(okProjects))
            .replace("{count}", String(exported))
            .replace("{dir}", picked) + overwrittenText + failuresText;
        msg.classList.remove("cfg-err");
      }
    }
  } finally {
    rpcExportAllProjectsBusy = false;
    // Re-rendering replaces the button node: restore both the current DOM button and the one that was clicked, for double safety
    const live = document.getElementById("rpc-export-all-projects") as HTMLButtonElement | null;
    for (const b of new Set([live, btn])) {
      if (!b) continue;
      b.disabled = false;
      b.textContent = t("rpcExportAllProjects");
    }
  }
}

/// Status chip: reuses the plugin tab's .plugin-status palette (-running green / -error red / base neutral),
/// mapping the three states ok/error/pending, without inventing new colors.
function rpcStatusChip(status: string): string {
  const cls = status === "ok" ? " plugin-status-running" : status === "error" ? " plugin-status-error" : "";
  const label = status === "ok" ? t("rpcTraceOk") : status === "error" ? t("rpcTraceFailed") : t("rpcTraceRunning");
  return `<span class="plugin-status${cls}">${esc(label)}</span>`;
}

/// /rpc trace row: summary expanded inline; meta carries the agent (rows are no longer grouped by agent, so without it the origin is unknown).
function rpcTraceRow(e: TraceEntry): string {
  // startedAt/endedAt are milliseconds (list_traces takes the row's ts, produced by now_ms), so the unit is ms
  // — consistent with the tree's `dur` of `Nms`; pending (no endedAt) shows ... (not the string undefined).
  const dur = e.endedAt ? `${e.endedAt - e.startedAt}ms` : "…";
  return `<details class="rec" data-rpc-trace="${esc(e.traceId)}" data-rpc-agent="${esc(e.agentId)}"><summary class="rec-head rec-mono"><span class="rec-title">${esc(e.traceId)}</span>${rpcStatusChip(e.status)}<span class="rec-sub">${esc(e.agentId)} · ${e.lineCount} ${esc(t("traceLines"))} · ${esc(formatBytes(e.sizeBytes))} · ${esc(dur)}</span></summary><div class="rec-detail" data-rpc-trace-body></div></details>`;
}

/// hook session row: same as above; hook files have only event rows and no root end -> endedAt is always absent,
/// so the row header shows the last activity time instead (lastTs is ms, formatLifecycleTime takes seconds, so it must be /1000).
function rpcHookRow(e: TraceEntry): string {
  const last = formatLifecycleTime(Math.floor((e.lastTs ?? e.endedAt ?? e.startedAt) / 1000));
  return `<details class="rec" data-hook-trace="${esc(e.traceId)}" data-rpc-agent="${esc(e.agentId)}"><summary class="rec-head rec-mono"><span class="rec-title">${esc(e.traceId)}</span><span class="rec-sub">${esc(e.agentId)} · ${e.lineCount} ${esc(t("traceLines"))} · ${esc(formatBytes(e.sizeBytes))} · ${esc(last)}</span></summary><div class="rec-detail" data-hook-trace-body></div></details>`;
}

/// Whole chain -> pasteable plain text: one line per record, two spaces per indent level, event prefix `· `,
/// attrs as compact JSON; spans without an end render their duration as .... Used by the 'Copy chain' button (pure function, easy to unit test).
function rpcTraceToText(lines: RpcTraceLine[]): string {
  // Same tree building as renderRpcTraceTree: get_rpc_trace returns in reverse order -> first reverse back to ascending time order
  const ordered = [...lines].reverse();
  const starts = new Map<string, Extract<RpcTraceLine, { ev: "start" }>>();
  const ends = new Map<string, Extract<RpcTraceLine, { ev: "end" }>>();
  const events = new Map<string, Extract<RpcTraceLine, { ev: "event" }>[]>();
  for (const l of ordered) {
    if (l.ev === "start") starts.set(l.spanId, l);
    else if (l.ev === "end") ends.set(l.spanId, l);
    else {
      const arr = events.get(l.spanId) ?? [];
      arr.push(l);
      events.set(l.spanId, arr);
    }
  }
  const children = new Map<string, string[]>();
  for (const [id, s] of starts) {
    const key = s.parentId ?? "";
    const arr = children.get(key) ?? [];
    arr.push(id);
    children.set(key, arr);
  }
  const out: string[] = [];
  const walk = (id: string, depth: number): void => {
    const s = starts.get(id);
    if (!s) return;
    const e = ends.get(id);
    const parts = [s.name, e ? `${e.ts - s.ts}ms` : "…"];
    if (e) {
      parts.push(e.status);
      if (e.error) parts.push(e.error);
    }
    parts.push(JSON.stringify(s.attrs));
    out.push(`${"  ".repeat(depth)}${parts.join("  ")}`);
    for (const ev of events.get(id) ?? []) {
      out.push(`${"  ".repeat(depth + 1)}· ${ev.name}  ${JSON.stringify(ev.attrs)}`);
    }
    for (const c of children.get(id) ?? []) walk(c, depth + 1);
  };
  for (const rootId of children.get("") ?? []) walk(rootId, 0);
  return out.join("\n");
}

/// hook session -> plain text: one line per event (name + timestamp + compact JSON), in the same order as the flat detail view.
function hookTraceToText(lines: RpcTraceLine[]): string {
  return lines
    .filter((l): l is Extract<RpcTraceLine, { ev: "event" }> => l.ev === "event")
    // The row's ts is ms and formatLifecycleTime takes seconds — the same /1000 as in the detail render
    .map((l) => `· ${l.name}  ${formatLifecycleTime(Math.floor(l.ts / 1000))}  ${JSON.stringify(l.attrs)}`)
    .join("\n");
}

/// Action row at the bottom of the detail: copy (human-readable text) / export (raw NDJSON for tools).
/// Buttons use .btn.ghost; the keyboard focus ring is provided by .op-content button.btn:focus-visible.
function rpcDetailActions(): string {
  return `<div class="rpc-actions"><button class="btn ghost" type="button" data-rpc-copy>${esc(t("rpcTraceCopy"))}</button><button class="btn ghost" type="button" data-rpc-export>${esc(t("rpcTraceExport"))}</button></div>`;
}

/// Export raw NDJSON: one record per line, in the same order as the on-disk file (the frontend's rows are in ascending time order = file order).
/// Uses Blob + a.click(), consistent with replay / audit export — no clipboard API dependency.
/// Failures outside onDone (Blob/download unavailable) show a failure message instead of staying silent.
function wireRpcExportButton(scope: HTMLElement, fileName: string, buildContent: () => string): void {
  const btn = scope.querySelector<HTMLButtonElement>("[data-rpc-export]");
  if (!btn) return;
  const label = btn.textContent ?? "";
  let restore = 0;
  btn.addEventListener("click", () => {
    let ok = true;
    try {
      const blob = new Blob([buildContent()], { type: "application/x-ndjson" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = fileName;
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      URL.revokeObjectURL(url);
    } catch {
      ok = false;
    }
    btn.textContent = ok ? t("rpcTraceExported") : t("rpcTraceExportFailed");
    window.clearTimeout(restore);
    restore = window.setTimeout(
      () => {
        btn.textContent = label;
      },
      ok ? 1500 : 2000,
    );
  });
}

/// trace rows -> NDJSON text (one record per line). input is already in file order (the caller reverses back to ascending first),
/// so stringify line by line: JSON.parse/stringify preserves key order, making it byte-identical to the on-disk content.
function linesToNdjson(lines: RpcTraceLine[]): string {
  return lines.map((l) => JSON.stringify(l)).join("\n") + "\n";
}

/// Copy feedback: prefer the app's own clipboard command (navigator.clipboard is often denied in the WebView),
/// and when both levels fail, explicitly show a failure message for 2s then restore — no more silently pretending success. The label is captured during wiring,
/// so repeated clicks don't lock 'Copied/Copy failed' in as the original text.
function wireRpcCopyButton(scope: HTMLElement, buildText: () => string): void {
  const btn = scope.querySelector<HTMLButtonElement>("[data-rpc-copy]");
  if (!btn) return;
  const label = btn.textContent ?? "";
  let restore = 0;
  btn.addEventListener("click", async () => {
    const text = buildText();
    let copied = false;
    try {
      await invoke("ui_clipboard_write", { text });
      copied = true;
    } catch {
      try {
        await navigator.clipboard.writeText(text);
        copied = true;
      } catch {
        copied = false;
      }
    }
    btn.textContent = copied ? t("rpcTraceCopied") : t("rpcTraceCopyFailed");
    window.clearTimeout(restore);
    restore = window.setTimeout(
      () => {
        btn.textContent = label;
      },
      copied ? 1500 : 2000,
    );
  });
}

/// /rpc detail: the span tree is fetched only on first expand (limit 500); on failure the flag is cleared so re-expanding retries.
async function openRpcTraceDetail(d: HTMLDetailsElement): Promise<void> {
  const target = d.querySelector<HTMLElement>("[data-rpc-trace-body]");
  if (!target || target.dataset.loaded === "1") return;
  target.dataset.loaded = "1"; // guards against concurrent double-fetch; cleared on failure
  target.innerHTML = `<p class="list-empty">${esc(t("traceLoading"))}</p>`;
  let lines: RpcTraceLine[] = [];
  try {
    lines = await invoke<RpcTraceLine[]>("get_rpc_trace", { agentId: d.dataset.rpcAgent ?? "", traceId: d.dataset.rpcTrace ?? "", limit: 500 });
  } catch (err) {
    target.dataset.loaded = "";
    target.innerHTML = `<p class="rpc-err">${esc(t("traceLoadFailed"))}: ${esc(String((err as Error).message ?? err))}</p>`;
    return;
  }
  // The API returns newest-first -> reverse back to file order (on disk it's ascending time); export uses this, the tree reverses internally.
  const ordered = [...lines].reverse();
  target.innerHTML = lines.length
    ? `<ul class="trace-lines rpc-tree">${renderRpcTraceTree(lines)}</ul>${rpcDetailActions()}`
    : `<p class="list-empty">${esc(t("traceEmpty"))}</p>`;
  if (lines.length) {
    wireRpcCopyButton(target, () => rpcTraceToText(lines));
    wireRpcExportButton(target, `${d.dataset.rpcTrace ?? "trace"}.ndjson`, () => linesToNdjson(ordered));
  }
}

/// hook session detail: fetch flat events on first expand (limit 300); render only rows where ev === 'event'.
async function openHookTraceDetail(d: HTMLDetailsElement): Promise<void> {
  const target = d.querySelector<HTMLElement>("[data-hook-trace-body]");
  if (!target || target.dataset.loaded === "1") return;
  target.dataset.loaded = "1";
  target.innerHTML = `<p class="list-empty">${esc(t("traceLoading"))}</p>`;
  let lines: RpcTraceLine[] = [];
  try {
    lines = await invoke<RpcTraceLine[]>("get_hook_trace", { agentId: d.dataset.rpcAgent ?? "", sessionId: d.dataset.hookTrace ?? "", limit: 300 });
  } catch (err) {
    target.dataset.loaded = "";
    target.innerHTML = `<p class="rpc-err">${esc(t("traceLoadFailed"))}: ${esc(String((err as Error).message ?? err))}</p>`;
    return;
  }
  // get_hook_trace returns newest-first (read_trace_at reverse order) -> reverse back to ascending time:
  // session logs should be read forward in time, and this stays consistent with the rpc tree (also reversed back to ascending).
  const ordered = [...lines].reverse();
  const rows = ordered
    .map((l) =>
      l.ev === "event"
        // The row's ts is ms (now_ms) and formatLifecycleTime takes seconds — it must be /1000,
        // otherwise feeding ms directly renders as 1970 (measured).
        ? `<li class="rpc-event">· ${esc(l.name)} <span class="trace-ts">${esc(formatLifecycleTime(Math.floor(l.ts / 1000)))}</span> <code class="trace-payload">${esc(JSON.stringify(l.attrs))}</code></li>`
        : "",
    )
    .join("");
  target.innerHTML = rows
    ? `<ul class="trace-lines rpc-tree">${rows}</ul>${rpcDetailActions()}`
    : `<p class="list-empty">${esc(t("traceEmpty"))}</p>`;
  if (rows) {
    wireRpcCopyButton(target, () => hookTraceToText(ordered));
    wireRpcExportButton(target, `${d.dataset.hookTrace ?? "session"}.ndjson`, () => linesToNdjson(ordered));
  }
}

function lifecycleKindLabel(kind: string): string {
  // plugin.lifecycle.starting -> starting, plugin.lifecycle.crashed -> crashed, ...
  switch (kind) {
    case "plugin.lifecycle.starting": return t("lifecycleKindStarting");
    case "plugin.lifecycle.running": return t("lifecycleKindRunning");
    case "plugin.lifecycle.stopped": return t("lifecycleKindStopped");
    case "plugin.lifecycle.crashed": return t("lifecycleKindCrashed");
    case "plugin.installed": return t("lifecycleKindInstalled");
    case "plugin.uninstalled": return t("lifecycleKindUninstalled");
    case "plugin.restarting": return t("lifecycleKindRestarting");
    default: return kind;
  }
}

function lifecycleClassFor(kind: string): string {
  if (kind.endsWith("crashed")) return "ev crashed";
  if (kind.endsWith("stopped")) return "ev stopped";
  if (kind.endsWith("running")) return "ev running";
  if (kind.endsWith("starting")) return "ev starting";
  if (kind.endsWith("installed")) return "ev installed";
  if (kind.endsWith("uninstalled")) return "ev uninstalled";
  if (kind.endsWith("restarting")) return "ev restarting";
  return "ev";
}

async function showLifecycleDialog(pluginId: string, pluginName: string): Promise<void> {
  const overlay = document.createElement("div");
  overlay.style.cssText =
    "position:fixed;inset:0;background:rgba(0,0,0,0.55);z-index:9999;display:flex;align-items:center;justify-content:center;backdrop-filter:blur(4px);";
  overlay.innerHTML = `
    <div class="install-dialog lifecycle-dialog" role="dialog" aria-label="${esc(t("lifecycleTitle"))}">
      <h2>${esc(t("lifecycleTitle"))}</h2>
      <div class="install-head">
        <div class="install-name">${esc(pluginName)}</div>
        <div class="install-id">${esc(pluginId)}</div>
      </div>
      <div class="lifecycle-loading">${esc(t("lifecycleLoading"))}</div>
    </div>`;
  document.body.appendChild(overlay);
  overlay.addEventListener("click", (ev) => {
    if (ev.target === overlay) overlay.remove();
  });
  let events: LifecycleEvent[] = [];
  try {
    events = await invoke<LifecycleEvent[]>("list_plugin_lifecycle", { id: pluginId, limit: 100 });
  } catch (err) {
    overlay.innerHTML = `
      <div class="install-dialog lifecycle-dialog" role="dialog" aria-label="${esc(t("lifecycleTitle"))}">
        <h2>${esc(t("lifecycleTitle"))}</h2>
        <p class="muted">${esc(t("lifecycleLoadFailed"))}: ${esc(String((err as Error).message ?? err))}</p>
        <div class="install-actions">
          <button class="btn ghost" id="lifecycle-close" type="button">${esc(t("installPreviewCancel"))}</button>
        </div>
      </div>`;
    overlay.querySelector("#lifecycle-close")?.addEventListener("click", () => overlay.remove());
    return;
  }
  const body = events.length
    ? `<ul class="lifecycle-list">${events
        .map(
          (e) =>
            `<li><span class="${lifecycleClassFor(e.kind)}">${esc(lifecycleKindLabel(e.kind))}</span><span class="lifecycle-time">${esc(formatLifecycleTime(e.timestamp))}</span>${e.reason ? `<span class="lifecycle-reason">${esc(e.reason)}</span>` : ""}</li>`,
        )
        .join("")}</ul>`
    : `<p class="muted">${esc(t("lifecycleEmpty"))}</p>`;
  overlay.innerHTML = `
    <div class="install-dialog lifecycle-dialog" role="dialog" aria-label="${esc(t("lifecycleTitle"))}">
      <h2>${esc(t("lifecycleTitle"))}</h2>
      <div class="install-head">
        <div class="install-name">${esc(pluginName)}</div>
        <div class="install-id">${esc(pluginId)} · ${events.length} ${esc(t("lifecycleCount"))}</div>
      </div>
      <div class="limeline-body">${body}</div>
      <div class="install-actions">
        <button class="btn ghost" id="lifecycle-close" type="button">${esc(t("installPreviewCancel"))}</button>
      </div>
    </div>`;
  overlay.querySelector("#lifecycle-close")?.addEventListener("click", () => overlay.remove());
}

/// Phase 37 — show the latest capability probe report. An optional 'Re-probe' triggers a backend run.
async function showProbeDialog(
  pluginId: string,
  pluginName: string,
  refreshParent: () => Promise<void>,
): Promise<void> {
  const overlay = document.createElement("div");
  overlay.style.cssText =
    "position:fixed;inset:0;background:rgba(0,0,0,0.55);z-index:9999;display:flex;align-items:center;justify-content:center;backdrop-filter:blur(4px);";
  overlay.innerHTML = `
    <div class="install-dialog probe-dialog" role="dialog" aria-label="${esc(t("probeTitle"))}">
      <h2>${esc(t("probeTitle"))}</h2>
      <div class="install-head">
        <div class="install-name">${esc(pluginName)}</div>
        <div class="install-id">${esc(pluginId)}</div>
      </div>
      <div class="probe-loading">${esc(t("probeLoading"))}</div>
    </div>`;
  document.body.appendChild(overlay);
  overlay.addEventListener("click", (ev) => {
    if (ev.target === overlay) overlay.remove();
  });

  const render = (report: ProbeReport | null, busy = false) => {
    if (!report) {
      overlay.innerHTML = `
        <div class="install-dialog probe-dialog" role="dialog" aria-label="${esc(t("probeTitle"))}">
          <h2>${esc(t("probeTitle"))}</h2>
          <div class="install-head">
            <div class="install-name">${esc(pluginName)}</div>
            <div class="install-id">${esc(pluginId)}</div>
          </div>
          <p class="muted">${esc(t("probeNever"))}</p>
          <div class="install-actions">
            <button class="btn" id="probe-run" type="button"${busy ? " disabled" : ""}>${esc(t("probeRun"))}</button>
            <button class="btn ghost" id="probe-close" type="button">${esc(t("installPreviewCancel"))}</button>
          </div>
        </div>`;
    } else {
      const stamp = new Date(report.ranAt * 1000).toLocaleString();
      const statusBadge = `<span class="probe-status probe-${esc(report.status)}">${esc(probeStatusLabel(report.status))}</span>`;
      const rows = report.capabilities.length
        ? `<ul class="probe-list">${report.capabilities
            .map(
              (c) =>
                `<li class="probe-row probe-row-${c.ok ? "ok" : "fail"}"><span class="probe-cap">${esc(c.capability)}</span><span class="probe-elapsed">${c.elapsedMs}ms</span>${c.error ? `<span class="probe-error">${esc(c.error)}</span>` : `<span class="probe-ok">✓</span>`}</li>`,
            )
            .join("")}</ul>`
        : `<p class="muted">${esc(t("probeNoCapabilities"))}</p>`;
      overlay.innerHTML = `
        <div class="install-dialog probe-dialog" role="dialog" aria-label="${esc(t("probeTitle"))}">
          <h2>${esc(t("probeTitle"))}</h2>
          <div class="install-head">
            <div class="install-name">${esc(pluginName)}</div>
            <div class="install-id">${esc(pluginId)}</div>
          </div>
          <div class="probe-summary">${statusBadge} <span class="probe-ran-at">${esc(stamp)}</span><div class="probe-text">${esc(report.summary)}</div></div>
          ${rows}
          <div class="install-actions">
            <button class="btn" id="probe-run" type="button"${busy ? " disabled" : ""}>${esc(t("probeRun"))}</button>
            <button class="btn ghost" id="probe-close" type="button">${esc(t("installPreviewCancel"))}</button>
          </div>
        </div>`;
    }
    overlay.querySelector("#probe-close")?.addEventListener("click", () => overlay.remove());
    overlay.querySelector("#probe-run")?.addEventListener("click", async () => {
      const btn = overlay.querySelector("#probe-run") as HTMLButtonElement | null;
      if (btn) btn.disabled = true;
      try {
        const fresh = await invoke<ProbeReport>("run_plugin_probe", { id: pluginId });
        await refreshParent();
        render(fresh, false);
      } catch (err) {
        const text = overlay.querySelector(".probe-text");
        if (text) text.textContent = `${t("probeFailed")}: ${(err as Error).message ?? err}`;
        if (btn) btn.disabled = false;
      }
    });
  };

  // Initially take the most recent one
  let report: ProbeReport | null = null;
  try {
    report = await invoke<ProbeReport | null>("get_probe_report", { id: pluginId });
  } catch {
    report = null;
  }
  render(report);
}

function probeStatusLabel(status: string): string {
  if (status === "passed") return t("probePassed");
  if (status === "failed") return t("probeFailed");
  return t("probeSkipped");
}

// ---- Phase 40 — Health config dialog (per-plugin watchdog / heartbeat strategy) -------

interface HealthConfig {
  heartbeatSec: number;
  pingTimeoutMs: number;
  maxRetries: number;
  backoffInitialMs: number;
  enabled: boolean;
}

const HEALTH_DEFAULTS: HealthConfig = {
  heartbeatSec: 0,
  pingTimeoutMs: 1000,
  maxRetries: 3,
  backoffInitialMs: 1000,
  enabled: true,
};

function clampHealth(n: number, lo: number, hi: number, fallback: number): number {
  if (!Number.isFinite(n)) return fallback;
  return Math.min(hi, Math.max(lo, Math.trunc(n)));
}

async function showHealthDialog(
  pluginId: string,
  pluginName: string,
  refreshParent: () => Promise<void>,
): Promise<void> {
  let cfg: HealthConfig = { ...HEALTH_DEFAULTS };
  try {
    cfg = await invoke<HealthConfig>("get_plugin_health_config", { id: pluginId });
  } catch {
    cfg = { ...HEALTH_DEFAULTS };
  }
  const overlay = document.createElement("div");
  overlay.style.cssText =
    "position:fixed;inset:0;background:rgba(0,0,0,0.55);z-index:9999;display:flex;align-items:center;justify-content:center;backdrop-filter:blur(4px);";
  overlay.innerHTML = `
    <div class="install-dialog health-dialog" role="dialog" aria-label="${esc(t("healthTitle"))}">
      <h2>${esc(t("healthTitle"))}</h2>
      <div class="install-head">
        <div class="install-name">${esc(pluginName)}</div>
        <div class="install-id">${esc(pluginId)}</div>
      </div>
      <p class="muted">${esc(t("healthHint"))}</p>
      <div class="health-row"><label>${esc(t("healthEnabled"))}</label><input type="checkbox" id="health-enabled"${cfg.enabled ? " checked" : ""}/></div>
      <div class="health-row"><label>${esc(t("healthHeartbeat"))}</label><input type="number" id="health-heartbeat" min="0" max="3600" value="${cfg.heartbeatSec}"/><span class="muted">${esc(t("healthSec"))}</span></div>
      <div class="health-row"><label>${esc(t("healthPingTimeout"))}</label><input type="number" id="health-timeout" min="100" max="60000" step="100" value="${cfg.pingTimeoutMs}"/><span class="muted">${esc(t("healthMs"))}</span></div>
      <div class="health-row"><label>${esc(t("healthMaxRetries"))}</label><input type="number" id="health-retries" min="0" max="100" value="${cfg.maxRetries}"/></div>
      <div class="health-row"><label>${esc(t("healthBackoff"))}</label><input type="number" id="health-backoff" min="0" max="30000" step="100" value="${cfg.backoffInitialMs}"/><span class="muted">${esc(t("healthMs"))}</span></div>
      <div class="install-actions">
        <button class="btn" id="health-save" type="button">${esc(t("healthSave"))}</button>
        <button class="btn ghost" id="health-reset" type="button">${esc(t("healthReset"))}</button>
        <button class="btn ghost" id="health-close" type="button">${esc(t("installPreviewCancel"))}</button>
      </div>
      <p class="muted health-msg" id="health-msg"></p>
    </div>`;
  document.body.appendChild(overlay);
  overlay.addEventListener("click", (ev) => {
    if (ev.target === overlay) overlay.remove();
  });
  overlay.querySelector("#health-close")?.addEventListener("click", () => overlay.remove());
  overlay.querySelector("#health-reset")?.addEventListener("click", () => {
    overlay.querySelector<HTMLInputElement>("#health-heartbeat")!.value = String(HEALTH_DEFAULTS.heartbeatSec);
    overlay.querySelector<HTMLInputElement>("#health-timeout")!.value = String(HEALTH_DEFAULTS.pingTimeoutMs);
    overlay.querySelector<HTMLInputElement>("#health-retries")!.value = String(HEALTH_DEFAULTS.maxRetries);
    overlay.querySelector<HTMLInputElement>("#health-backoff")!.value = String(HEALTH_DEFAULTS.backoffInitialMs);
    overlay.querySelector<HTMLInputElement>("#health-enabled")!.checked = HEALTH_DEFAULTS.enabled;
  });
  overlay.querySelector("#health-save")?.addEventListener("click", async () => {
    const enabled = overlay.querySelector<HTMLInputElement>("#health-enabled")!.checked;
    const heartbeat = Number(overlay.querySelector<HTMLInputElement>("#health-heartbeat")!.value);
    const timeout = Number(overlay.querySelector<HTMLInputElement>("#health-timeout")!.value);
    const retries = Number(overlay.querySelector<HTMLInputElement>("#health-retries")!.value);
    const backoff = Number(overlay.querySelector<HTMLInputElement>("#health-backoff")!.value);
    const next: HealthConfig = {
      heartbeatSec: clampHealth(heartbeat, 0, 3600, HEALTH_DEFAULTS.heartbeatSec),
      pingTimeoutMs: clampHealth(timeout, 100, 60000, HEALTH_DEFAULTS.pingTimeoutMs),
      maxRetries: clampHealth(retries, 0, 100, HEALTH_DEFAULTS.maxRetries),
      backoffInitialMs: clampHealth(backoff, 0, 30000, HEALTH_DEFAULTS.backoffInitialMs),
      enabled,
    };
    const saveBtn = overlay.querySelector<HTMLButtonElement>("#health-save");
    if (saveBtn) saveBtn.disabled = true;
    try {
      await invoke("set_plugin_health_config", { id: pluginId, cfg: next });
      const msg = overlay.querySelector("#health-msg");
      if (msg) msg.textContent = t("healthSaved");
      await refreshParent();
    } catch (err) {
      const msg = overlay.querySelector("#health-msg");
      if (msg) msg.textContent = `${t("healthSaveFailed")}: ${(err as Error).message ?? err}`;
      if (saveBtn) saveBtn.disabled = false;
    }
  });
}

// ---- Phase 39 — Hotkeys tab (global hotkeys + command palette) ------------------------

interface HotkeyBinding {
  combo: string;
  action: HotkeyAction;
  enabled: boolean;
  registered_at_os: boolean;
}

interface HotkeyResult {
  binding: HotkeyBinding;
  registered: boolean;
  error?: string;
}

const BUILTIN_ACTION_OPTIONS: Array<{ key: string; i18nKey: string }> = [
  { key: "toggle-pet", i18nKey: "hotkeyActionTogglePet" },
  { key: "open-settings", i18nKey: "hotkeyActionOpenSettings" },
  { key: "open-palette", i18nKey: "hotkeyActionOpenPalette" },
  { key: "quit", i18nKey: "hotkeyActionQuit" },
];

function formatHotkeyAction(a: HotkeyAction): string {
  if (a.kind === "builtin") {
    const opt = BUILTIN_ACTION_OPTIONS.find((o) => o.key === a.action);
    return opt ? t(opt.i18nKey as never) : (a.action ?? "?");
  }
  return `${a.plugin_id} → ${a.capability}`;
}

/// Current list snapshot: the rebind guard, overwrite confirmation, and restore-default prompt all rely on it; updated on refreshHotkeys.
let currentBindings: HotkeyBinding[] = [];

/// Aligned with the backend normalize_combo (for the frontend guard: BTreeSet lexicographic order Alt < CmdOrCtrl < Shift < Super).
/// `'cmdorctrl'` catches an already-normalized string (idempotent): the picker and restore-default feed normalized strings back in, and without it you get garbage like Shift+CMDORCTRL+P.
function normalizeComboTs(raw: string): string {
  const mods = new Set<string>();
  const keys: string[] = [];
  for (const part of raw.split("+")) {
    const p = part.trim();
    if (!p) continue;
    const low = p.toLowerCase();
    if (["ctrl", "control", "cmd", "command", "cmdorctrl"].includes(low)) mods.add("CmdOrCtrl");
    else if (low === "alt" || low === "option") mods.add("Alt");
    else if (low === "shift") mods.add("Shift");
    else if (["super", "meta", "win", "windows"].includes(low)) mods.add("Super");
    else keys.push(p.toUpperCase());
  }
  keys.sort();
  return [...[...mods].sort(), ...keys].join("+");
}

const IS_MAC = typeof navigator !== "undefined" && /Mac|iP(hone|ad|od)/.test(navigator.userAgent);

/// Presentation polish: storage is always the normalized string; only the display is swapped to platform-conventional symbols.
function prettifyCombo(combo: string): string {
  return combo
    .split("+")
    .map((tok) => {
      if (tok === "CmdOrCtrl") return IS_MAC ? "⌘" : "Ctrl";
      if (tok === "Shift") return "⇧";
      if (tok === "Alt") return IS_MAC ? "⌥" : "Alt";
      return tok;
    })
    .join("+");
}

function sameAction(a: HotkeyAction | undefined, b: HotkeyAction | undefined): boolean {
  if (!a || !b || a.kind !== b.kind) return false;
  return a.kind === "builtin"
    ? a.action === b.action
    : a.plugin_id === b.plugin_id && a.capability === b.capability;
}

function setHotkeyMsg(text: string): void {
  const m = document.getElementById("hotkey-msg");
  if (m) m.textContent = text;
}

function reportSetResult(combo: string, r: HotkeyResult): void {
  if (r.registered) setHotkeyMsg(`✓ ${prettifyCombo(combo)}`);
  else if (r.error) setHotkeyMsg(`⚠ ${t("hotkeySavedNotActive")}: ${r.error}`);
  else setHotkeyMsg(`⚠ ${t("hotkeyNotActiveHint")}`);
}

/// Recorder key normalization: fixes arrow keys / Shift+digits / named-key casing; returns null = cannot map.
const NAMED_KEYS: Record<string, string> = {
  ArrowUp: "Up",
  ArrowDown: "Down",
  ArrowLeft: "Left",
  ArrowRight: "Right",
  " ": "Space",
  Tab: "Tab",
  Enter: "Enter",
  Backspace: "Backspace",
  Delete: "Delete",
  Home: "Home",
  End: "End",
  PageUp: "PageUp",
  PageDown: "PageDown",
  Insert: "Insert",
};
const F_KEY_RE = /^F([1-9]|1[0-2])$/;

function keyNameForCombo(ev: KeyboardEvent): { key: string; isFKey: boolean } | null {
  if (ev.isComposing) return null;
  const k = ev.key;
  if (NAMED_KEYS[k] !== undefined) return { key: NAMED_KEYS[k], isFKey: false };
  if (F_KEY_RE.test(k)) return { key: k, isFKey: true };
  if (k.length === 1 && /[a-z]/i.test(k)) return { key: k.toUpperCase(), isFKey: false };
  // Shift+digit: key is something like '!', but code is still Digit1 — use code to recover the digit key.
  const codeMatch = /^(?:Digit|Numpad)([0-9])$/.exec(ev.code);
  if (codeMatch) return { key: codeMatch[1], isFKey: false };
  return null; // punctuation / dead keys / other layout chars: unsupported
}

/// Recorder core: keyboard event -> modifier progress / unsupported / missing modifier / combo (raw, not normalized).
/// Rules are exactly the same as the original dialog recorder (a modifier pressed alone is not recorded; F1-F12 may have no modifier, everything else needs at least one),
/// only the consumer changed from a dialog to an inline pill.
type HotkeyCaptureOutcome =
  | { kind: "modifier"; mods: string[] }
  | { kind: "unsupported" }
  | { kind: "need-modifier" }
  | { kind: "combo"; combo: string };

function comboFromKeyEvent(ev: KeyboardEvent): HotkeyCaptureOutcome {
  const mods: string[] = [];
  if (ev.ctrlKey || ev.metaKey) mods.push("Ctrl");
  if (ev.altKey) mods.push("Alt");
  if (ev.shiftKey) mods.push("Shift");
  // A modifier pressed alone is not recorded, only the progress display updates
  if (["Control", "Shift", "Alt", "Meta"].includes(ev.key)) return { kind: "modifier", mods };
  const mapped = keyNameForCombo(ev);
  if (!mapped) return { kind: "unsupported" };
  // F1-F12 may have no modifier; everything else requires at least one modifier
  if (mods.length === 0 && !mapped.isFKey) return { kind: "need-modifier" };
  return { kind: "combo", combo: [...mods, mapped.key].join("+") };
}

/// Row data: palette action + display label; the binding is looked up on the fly from currentBindings by sameAction.
interface HotkeyRow {
  action: HotkeyAction;
  label: string;
}

/// Render item: palette row / orphan row; orphan rows carry their source binding, and the binding a palette row claims is also recorded here.
interface HotkeyItem {
  row: HotkeyRow;
  orphan: boolean;
  binding?: HotkeyBinding;
}

/// Teardown function for the pill being recorded (at most one at a time); refresh calls it first to avoid stale keydown listeners.
let endHotkeyCapture: (() => void) | null = null;

function cancelHotkeyCapture(): void {
  endHotkeyCapture?.();
}

async function refreshHotkeys(): Promise<void> {
  const box = document.getElementById("hotkey-list");
  if (!box) return;
  cancelHotkeyCapture();
  try {
    currentBindings = await invoke<HotkeyBinding[]>("list_hotkeys");
  } catch (err) {
    box.innerHTML = `<p class="setting-hint">✗ ${esc(String(err))}</p>`;
    return;
  }
  let entries: PaletteEntry[] = [];
  try {
    entries = await invoke<PaletteEntry[]>("list_palette_entries");
  } catch {
    entries = [];
  }
  // Row = palette LEFT JOIN bindings (matched by sameAction): builtins in fixed order, plugins by plugin -> capability.
  const builtinRank = (action: HotkeyAction): number => {
    const i = BUILTIN_ACTION_OPTIONS.findIndex((o) => o.key === action.action);
    return i < 0 ? BUILTIN_ACTION_OPTIONS.length : i;
  };
  const toRow = (e: PaletteEntry): HotkeyRow => {
    // A builtin's action is the `builtin:` suffix of its id (aligned with the backend BuiltinAction's kebab-case).
    const action: HotkeyAction =
      e.kind === "builtin"
        ? { kind: "builtin", action: e.id.replace(/^builtin:/, "") as HotkeyAction["action"] }
        : { kind: "plugin", plugin_id: e.plugin_id ?? "", capability: e.capability ?? "" };
    // builtin labels go through i18n; plugins use the palette title (the backend already assembles `plugin -> capability`).
    return { action, label: e.kind === "builtin" ? formatHotkeyAction(action) : e.title };
  };
  const rows = [
    ...entries
      .filter((e) => e.kind === "builtin")
      .map(toRow)
      .sort((a, b) => builtinRank(a.action) - builtinRank(b.action)),
    ...entries
      .filter((e) => e.kind === "plugin")
      .map(toRow)
      .sort(
        (a, b) =>
          (a.action.plugin_id ?? "").localeCompare(b.action.plugin_id ?? "") ||
          (a.action.capability ?? "").localeCompare(b.action.capability ?? ""),
      ),
  ];
  // Row = palette LEFT JOIN bindings; each palette row claims the first binding with the same action.
  // Unclaimed bindings (orphans after a plugin uninstall, or extra combos with the same action in old data) are appended after the separator.
  const claimed = new Set<HotkeyBinding>();
  const items: HotkeyItem[] = rows.map((row) => {
    const binding = currentBindings.find((x) => !claimed.has(x) && sameAction(x.action, row.action));
    if (binding) claimed.add(binding);
    return { row, orphan: false, binding };
  });
  const orphans = currentBindings.filter((b) => !claimed.has(b));
  items.push(
    ...orphans.map((b) => ({
      row: { action: b.action, label: formatHotkeyAction(b.action) },
      orphan: true,
      binding: b,
    })),
  );

  const rowHtml = (item: HotkeyItem, index: number): string => {
    const { row, orphan, binding: b } = item;
    const notActive = !!b && b.enabled && !b.registered_at_os;
    const pillClass = `hotkey-pill${b ? " bound" : ""}${notActive ? " warn" : ""}`;
    const pillText = b ? prettifyCombo(b.combo) : t("hotkeyNoBinding");
    const pillTitle = notActive ? t("hotkeyNotActiveHint") : b ? t("hotkeyRebind") : t("hotkeyPressKeys");
    const pill = `<button class="${pillClass}" data-hotkey-row="${index}" data-combo="${esc(b?.combo ?? "")}" title="${esc(pillTitle)}" type="button">${esc(pillText)}</button>`;
    const toggle = b
      ? `<button class="btn ghost" data-hotkey-toggle="${esc(b.combo)}" data-next="${b.enabled ? "0" : "1"}" type="button">${esc(b.enabled ? t("hotkeyDisable") : t("hotkeyEnable"))}</button>`
      : "";
    let badge = "";
    if (b && b.enabled && !b.registered_at_os) {
      badge = `<span class="hotkey-badge">${esc(t("hotkeyNotActive"))}</span><button class="btn ghost" data-hotkey-retry="${esc(b.combo)}" type="button">${esc(t("hotkeyRetry"))}</button>`;
    }
    // Orphan rows keep an explicit delete button (no palette action to lean on); normal rows are cleared with the pill's Delete.
    const del =
      orphan && b
        ? `<button class="btn ghost danger" data-hotkey-del="${esc(b.combo)}" type="button">${esc(t("remove"))}</button>`
        : "";
    return `<div class="sess hotkey-row${b && !b.enabled ? " inactive" : ""}" data-hotkey-combo="${esc(b?.combo ?? "")}"><span class="hotkey-label">${esc(row.label)}</span><span class="hotkey-actions">${pill}${toggle}${badge}${del}</span></div>`;
  };

  const divider = `<div class="hotkey-orphan-divider"><span>${esc(t("hotkeyOrphaned"))}</span></div>`;
  box.innerHTML =
    items.length === 0
      ? `<p class="empty">${esc(t("hotkeysNone"))}</p>`
      : items
          .map((item, i) => (i === rows.length && orphans.length > 0 ? divider : "") + rowHtml(item, i))
          .join("");

  box.querySelectorAll<HTMLButtonElement>("button.hotkey-pill").forEach((pill) => {
    pill.addEventListener("click", () => {
      const item = items[Number(pill.dataset.hotkeyRow ?? "-1")];
      if (!item) return;
      beginHotkeyCapture(pill, item.row, item.binding?.combo ?? "");
    });
  });
  box.querySelectorAll<HTMLButtonElement>("button[data-hotkey-del]").forEach((btn) => {
    btn.addEventListener("click", () => void deleteHotkeyBinding(btn.dataset.hotkeyDel ?? ""));
  });
  box.querySelectorAll<HTMLButtonElement>("button[data-hotkey-toggle]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const combo = btn.dataset.hotkeyToggle ?? "";
      const enabled = btn.dataset.next === "1";
      try {
        const r = await invoke<HotkeyResult>("set_hotkey_enabled", { combo, enabled });
        reportSetResult(combo, r);
      } catch (err) {
        setHotkeyMsg(`✗ ${String(err)}`);
      }
      await refreshHotkeys();
    });
  });
  box.querySelectorAll<HTMLButtonElement>("button[data-hotkey-retry]").forEach((btn) => {
    btn.addEventListener("click", () => void retryHotkey(btn.dataset.hotkeyRetry ?? ""));
  });
}

/// Clear a binding (normal rows via the pill's Delete/Backspace, orphan rows via the delete button).
async function deleteHotkeyBinding(combo: string): Promise<void> {
  if (!combo) return;
  try {
    await invoke("delete_hotkey", { combo });
  } catch (err) {
    setHotkeyMsg(`✗ ${String(err)}`);
  }
  await refreshHotkeys();
}

/// Write the recorded combo to the row's action: confirm first if it collides with another action (after overwriting, the other becomes unbound);
/// if the combo changed, delete the old combo so an action keeps only one binding.
async function saveHotkeyBinding(row: HotkeyRow, combo: string, oldCombo: string): Promise<void> {
  const clash = currentBindings.find((b) => b.combo === combo && !sameAction(b.action, row.action));
  if (clash && !window.confirm(t("hotkeyOverwriteConfirm"))) {
    await refreshHotkeys();
    return;
  }
  try {
    const r = await invoke<HotkeyResult>("set_hotkey", { combo, action: row.action });
    reportSetResult(r.binding.combo, r);
    if (oldCombo && oldCombo !== combo) {
      await invoke("delete_hotkey", { combo: oldCombo });
    }
  } catch (err) {
    setHotkeyMsg(`✗ ${String(err)}`);
  }
  await refreshHotkeys();
}

/// Inline pill recording state: Esc cancels, Delete/Backspace clears, valid combos go to saveHotkeyBinding.
function beginHotkeyCapture(pill: HTMLButtonElement, row: HotkeyRow, oldCombo: string): void {
  cancelHotkeyCapture();
  const restingText = pill.textContent ?? "";
  let restoreTimer: number | undefined;
  const paint = (text: string) => {
    pill.textContent = text;
  };
  const end = () => {
    window.removeEventListener("keydown", onKey, true);
    if (restoreTimer !== undefined) window.clearTimeout(restoreTimer);
    restoreTimer = undefined;
    pill.classList.remove("capturing");
    paint(restingText);
    if (endHotkeyCapture === end) endHotkeyCapture = null;
  };
  // Unsupported / missing modifier: a brief hint inside the pill, returning to the recording prompt after 1.2s.
  const showTransient = (text: string) => {
    paint(text);
    if (restoreTimer !== undefined) window.clearTimeout(restoreTimer);
    restoreTimer = window.setTimeout(() => paint(t("hotkeyPressKeys")), 1200);
  };
  const onKey = (ev: KeyboardEvent) => {
    ev.preventDefault();
    ev.stopPropagation();
    if (ev.key === "Escape") {
      end();
      return;
    }
    if (ev.key === "Delete" || ev.key === "Backspace") {
      end();
      void deleteHotkeyBinding(oldCombo);
      return;
    }
    const outcome = comboFromKeyEvent(ev);
    if (outcome.kind === "modifier") {
      paint(outcome.mods.length ? `${outcome.mods.join("+")}+…` : t("hotkeyPressKeys"));
      return;
    }
    if (outcome.kind === "unsupported") {
      showTransient(t("hotkeyUnsupportedKey"));
      return;
    }
    if (outcome.kind === "need-modifier") {
      showTransient(t("hotkeyRecordNeedModifier"));
      return;
    }
    end();
    void saveHotkeyBinding(row, normalizeComboTs(outcome.combo), oldCombo);
  };
  pill.classList.add("capturing");
  paint(t("hotkeyPressKeys"));
  endHotkeyCapture = end;
  window.addEventListener("keydown", onKey, true);
}

/// Retry registration = call set_hotkey again with the current action (idempotent upsert + re-register).
async function retryHotkey(combo: string): Promise<void> {
  const b = currentBindings.find((x) => x.combo === combo);
  if (!b) return;
  try {
    const r = await invoke<HotkeyResult>("set_hotkey", { combo, action: b.action });
    reportSetResult(r.binding.combo, r);
  } catch (err) {
    setHotkeyMsg(`✗ ${String(err)}`);
  }
  await refreshHotkeys();
}

const DEFAULT_HOTKEYS: Array<[string, HotkeyAction]> = [
  ["CmdOrCtrl+Shift+P", { kind: "builtin", action: "open-palette" }],
  ["CmdOrCtrl+Shift+,", { kind: "builtin", action: "open-settings" }],
  ["CmdOrCtrl+Shift+H", { kind: "builtin", action: "toggle-pet" }],
];

/// Restore default: upserts only these three combos; confirm first if non-default actions are already bound.
/// NOTE: default items the user disabled are re-enabled (upsert writes enabled:true); expected behavior.
async function restoreDefaultHotkeys(): Promise<void> {
  const overwritten = currentBindings.filter((b) => {
    const d = DEFAULT_HOTKEYS.find(([c]) => normalizeComboTs(c) === b.combo);
    return d && !sameAction(d[1], b.action);
  });
  if (overwritten.length > 0 && !window.confirm(t("hotkeyRestoreConfirm"))) return;
  for (const [combo, action] of DEFAULT_HOTKEYS) {
    try {
      const r = await invoke<HotkeyResult>("set_hotkey", { combo, action });
      reportSetResult(combo, r);
    } catch (err) {
      setHotkeyMsg(`✗ ${combo}: ${String(err)}`);
    }
  }
  await refreshHotkeys();
}

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
function startInstallAskListener(): void {
  void listen<{ id: string; pluginId: string; permission: string; canAlways?: boolean }>(
    "opencapx-install-ask",
    (ev) => showInstallAskModal(ev.payload),
  );
  // The other side (pet bubble) answered -> collapse this side, so a now-invalid button isn't left behind
  void listen("opencapx-install-ask-done", () => closeInstallAskModal());
}

function startHotkeyPaletteListener(): void {
  // Global hotkey::open_palette event arrives -> open the command palette modal.
  onEvent("hotkey::open_palette", () => {
    void openCommandPalette();
  });
}

async function openCommandPalette(): Promise<void> {
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
            return `<li class="palette-row" data-pal-id="${esc(e.id)}" data-pal-kind="${esc(e.kind)}" data-pal-action="${esc(derivedAction)}" data-pal-plugin="${esc(e.plugin_id ?? "")}" data-pal-cap="${esc(e.capability ?? "")}"><span class="palette-row-title">${esc(e.title)}</span><span class="palette-row-kind">${esc(e.kind)}</span></li>`;
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

function showUninstallPreviewDialog(p: UninstallPreview): Promise<boolean> {
  return new Promise((resolve) => {
    const overlay = document.createElement("div");
    overlay.style.cssText =
      "position:fixed;inset:0;background:rgba(0,0,0,0.55);z-index:9999;display:flex;align-items:center;justify-content:center;backdrop-filter:blur(4px);";
    const depList = p.dependents.length
      ? `<ul class="uninstall-deps">${p.dependents.map((d) => `<li><code>${esc(d)}</code></li>`).join("")}</ul>`
      : `<p class="muted">${esc(t("uninstallNoDependents"))}</p>`;
    overlay.innerHTML = `
      <div class="install-dialog" role="dialog" aria-label="${esc(t("pluginUninstall"))}">
        <h2>${esc(t("pluginUninstall"))}</h2>
        <div class="install-head">
          <div class="install-name">${esc(p.name)}</div>
          <div class="install-id">${esc(p.id)} · v${esc(p.version)}</div>
        </div>
        <div class="uninstall-summary">
          <div class="uninstall-row"><span>${esc(t("uninstallAutoReload"))}</span><b>${p.autoReload ? esc(t("uninstallYes")) : esc(t("uninstallNo"))}</b></div>
          <div class="uninstall-row"><span>${esc(t("uninstallConfig"))}</span><b>${p.configExists ? esc(t("uninstallYes")) : esc(t("uninstallNo"))}</b></div>
          <div class="uninstall-row"><span>${esc(t("uninstallPermissionCount"))}</span><b>${p.permissionCount}</b></div>
          <div class="uninstall-row"><span>${esc(t("uninstallCapabilityCount"))}</span><b>${p.capabilityCount}</b></div>
        </div>
        <div class="install-section">
          <div class="install-section-title">${esc(t("uninstallDependents"))}</div>
          ${depList}
        </div>
        <p class="muted">${esc(t("uninstallHint"))}</p>
        <div class="install-actions">
          <button class="btn ghost" id="uninstall-cancel" type="button">${esc(t("installPreviewCancel"))}</button>
          <button class="btn danger" id="uninstall-confirm" type="button">${esc(t("pluginUninstall"))}</button>
        </div>
      </div>`;
    document.body.appendChild(overlay);
    const close = (v: boolean) => {
      overlay.remove();
      resolve(v);
    };
    overlay.querySelector("#uninstall-cancel")?.addEventListener("click", () => close(false));
    overlay.querySelector("#uninstall-confirm")?.addEventListener("click", () => close(true));
    overlay.addEventListener("click", (ev) => {
      if (ev.target === overlay) close(false);
    });
  });
}

/// F7 — generic warning confirmation dialog (reused by key-change / unsigned confirmation).
function showWarningConfirmDialog(opts: {
  title: string;
  body: string;
  confirmLabel: string;
}): Promise<boolean> {
  return new Promise((resolve) => {
    const overlay = document.createElement("div");
    overlay.id = "warn-confirm-overlay";
    overlay.style.cssText =
      "position:fixed;inset:0;background:rgba(0,0,0,0.55);z-index:9999;display:flex;align-items:center;justify-content:center;backdrop-filter:blur(4px);";
    overlay.innerHTML = `
      <div class="install-dialog" role="dialog" aria-label="${esc(opts.title)}">
        <h2>${esc(opts.title)}</h2>
        <p class="install-desc">${esc(opts.body)}</p>
        <div class="install-actions">
          <button class="btn ghost" id="warn-cancel" type="button">${esc(t("installPreviewCancel"))}</button>
          <button class="btn primary" id="warn-confirm" type="button">${esc(opts.confirmLabel)}</button>
        </div>
      </div>`;
    document.body.appendChild(overlay);
    const close = (v: boolean) => {
      overlay.remove();
      resolve(v);
    };
    overlay.querySelector("#warn-cancel")?.addEventListener("click", () => close(false));
    overlay.querySelector("#warn-confirm")?.addEventListener("click", () => close(true));
    overlay.addEventListener("click", (ev) => {
      if (ev.target === overlay) close(false);
    });
  });
}

function showInstallPreviewDialog(
  p: PluginPreview,
  opts?: {
    mode?: "install" | "update";
    publisherChange?: { from?: string | null; to?: string | null } | null;
  },
): Promise<boolean> {
  return new Promise((resolve) => {
    const overlay = document.createElement("div");
    overlay.id = "install-preview-overlay";
    overlay.style.cssText =
      "position:fixed;inset:0;background:rgba(0,0,0,0.55);z-index:9999;display:flex;align-items:center;justify-content:center;backdrop-filter:blur(4px);";
    const mode = opts?.mode ?? "install";
    const status = p.signature.status;
    const softWarn = status === "unsigned" || status === "unknown-key";
    const hardDeny =
      status === "tampered" || status === "bad-signature" || status === "malformed-signature";
    const caps = p.capabilities.length
      ? p.capabilities.map((c) => `<span class="cap-badge">${esc(c)}</span>`).join("")
      : `<span class="muted">—</span>`;
    const perms = p.permissions.length
      ? p.permissions
          .map(
            (perm) =>
              `<li class="install-perm${perm.highRisk ? " high-risk" : ""}"><span>${esc(perm.name)}</span>${
                perm.highRisk ? `<span class="risk-tag">${esc(t("installPreviewHighRisk"))}</span>` : ""
              }</li>`,
          )
          .join("")
      : `<li class="muted">—</li>`;
    const desc = p.description
      ? `<p class="install-desc">${esc(p.description)}</p>`
      : `<p class="install-desc muted">${esc(t("installPreviewNoDescription"))}</p>`;
    const verifiedBadge = p.official
      ? `<span class="install-verified official">${esc(t("pluginOfficial"))}</span>`
      : p.verified === true
        ? `<span class="install-verified">✓ ${esc(t("pluginVerified"))}</span>`
        : "";
    const compatHtml = p.compat.ok
      ? ""
      : `<div class="plugin-desc warn">${esc(t("pluginCompatWarn"))}: minCore ${esc(p.compat.minCoreVersion ?? "?")} &gt; ${esc(p.compat.current)}</div>`;
    const diff = p.permissionDiff;
    const diffHtml =
      diff && (diff.added.length || diff.removed.length)
        ? `<div class="install-section">
            <div class="install-section-title">${esc(t("pluginPermDiff"))}</div>
            <ul class="install-perms">
              ${diff.added.map((x) => `<li class="install-perm"><span>+ ${esc(x)}</span></li>`).join("")}
              ${diff.removed.map((x) => `<li class="install-perm muted"><span>− ${esc(x)}</span></li>`).join("")}
            </ul>
          </div>`
        : "";
    const publisherHtml = opts?.publisherChange
      ? `<div class="plugin-desc warn">${esc(t("pluginPublisherChangeBody"))} (${esc(opts.publisherChange.from ?? "—")} → ${esc(opts.publisherChange.to ?? "—")})</div>`
      : "";
    // The unsigned confirmation checkbox sits **right next to the confirm button**: the text stays in the explanation area above, the checkbox + hint go into the button area.
    // When placed in the middle of the dialog, a long dialog (capability/permission list) pushes it out of view, and the user sees only an unclickable
    // gray 'Install' — that is exactly where 'I clicked and nothing happened' comes from.
    const softWarnHtml = softWarn
      ? `<div class="plugin-desc warn">⚠ ${esc(t("pluginUnsignedWarn"))}</div>`
      : "";
    const ackHtml = softWarn
      ? `<label class="install-ack-row"><input type="checkbox" id="install-ack"/><span>${esc(t("pluginUnsignedAck"))}</span></label>
         <p class="install-desc" id="install-ack-hint">${esc(t("pluginUnsignedAckHint"))}</p>`
      : "";
    const hardDenyHtml = hardDeny
      ? `<div class="plugin-desc warn">⛔ ${esc(t("pluginHardDeny"))}</div>`
      : "";
    // S5c — unsigned + sandbox declared: forced execution after install (macOS); tell the user in the dialog first.
    const sandboxForcedHtml =
      p.sandboxDeclared && softWarn
        ? `<div class="plugin-desc warn">${esc(t("installSandboxForced"))}</div>`
        : "";
    // review F1 — unverified and no sandbox declared: must not be silent; clearly state it will run with full user permissions.
    const unconfinedHtml =
      p.verified !== true && !p.sandboxDeclared
        ? `<div class="plugin-desc warn">${esc(t("installSandboxUnconfined"))}</div>`
        : "";
    overlay.innerHTML = `
      <div class="install-dialog" role="dialog" aria-label="${esc(t("installPreviewTitle"))}">
        <h2>${esc(mode === "update" ? t("pluginUpdate") : t("installPreviewTitle"))}</h2>
        <div class="install-head">
          <div class="install-name">${esc(p.name)}</div>
          <div class="install-id">${esc(p.id)} · v${esc(p.version)} · ${esc(p.type)}${verifiedBadge ? ` · ${verifiedBadge}` : ""}</div>
          <div class="install-sig">${signatureBadgeHtml(p.signature)}</div>
        </div>
        ${desc}
        ${publisherHtml}
        ${compatHtml}
        ${softWarnHtml}
        ${sandboxForcedHtml}
        ${unconfinedHtml}
        ${hardDenyHtml}
        <div class="install-section">
          <div class="install-section-title">${esc(t("installPreviewCapabilities"))}</div>
          <div class="install-caps">${caps}</div>
        </div>
        <div class="install-section">
          <div class="install-section-title">${esc(t("installPreviewPermissions"))}</div>
          <ul class="install-perms">${perms}</ul>
        </div>
        ${diffHtml}
        <div class="install-actions">
          ${ackHtml}
          <div class="install-buttons">
            <button class="btn ghost" id="install-preview-cancel" type="button">${esc(t("installPreviewCancel"))}</button>
            ${
              hardDeny
                ? ""
                : `<button class="btn primary" id="install-preview-confirm" type="button" ${softWarn ? "disabled" : ""}>${esc(mode === "update" ? t("pluginUpdate") : t("installPreviewConfirm"))}</button>`
            }
          </div>
        </div>
      </div>`;
    document.body.appendChild(overlay);
    const confirmBtn = overlay.querySelector<HTMLButtonElement>("#install-preview-confirm");
    const ack = overlay.querySelector<HTMLInputElement>("#install-ack");
    ack?.addEventListener("change", () => {
      if (confirmBtn) confirmBtn.disabled = !ack.checked;
      // When unchecked, put 'why the button won't click' right next to it; it collapses once checked
      const hint = overlay.querySelector<HTMLElement>("#install-ack-hint");
      if (hint) hint.hidden = ack.checked;
    });
    const close = (v: boolean) => {
      overlay.remove();
      resolve(v);
    };
    overlay.querySelector("#install-preview-cancel")?.addEventListener("click", () => close(false));
    confirmBtn?.addEventListener("click", () => close(true));
    overlay.addEventListener("click", (ev) => {
      if (ev.target === overlay) close(false);
    });
  });
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

// Phase 32 — capability dependency graph. Nodes = installed plugins (circle + id), edges = shared capabilities (lines + hover highlight).
interface DependencyNode {
  id: string;
  name: string;
  capabilities: string[];
}
interface DependencyEdge {
  from: string;
  to: string;
  shared: string[];
}
interface DependencyGraph {
  nodes: DependencyNode[];
  edges: DependencyEdge[];
}

function renderDepGraph(graph: DependencyGraph): string {
  const n = graph.nodes.length;
  if (n === 0) return `<p class="empty">${esc(t("depGraphEmpty"))}</p>`;
  const w = 520;
  const h = 360;
  const cx = w / 2;
  const cy = h / 2;
  const r = Math.min(w, h) / 2 - 60;
  // Nodes evenly distributed around the circle
  const pos = new Map<string, { x: number; y: number }>();
  graph.nodes.forEach((nd, i) => {
    const angle = (i / n) * Math.PI * 2 - Math.PI / 2;
    pos.set(nd.id, { x: cx + r * Math.cos(angle), y: cy + r * Math.sin(angle) });
  });
  // Edges
  const edges = graph.edges
    .map((e) => {
      const a = pos.get(e.from);
      const b = pos.get(e.to);
      if (!a || !b) return "";
      return `<line class="dep-edge" data-from="${esc(e.from)}" data-to="${esc(e.to)}" x1="${a.x.toFixed(1)}" y1="${a.y.toFixed(1)}" x2="${b.x.toFixed(1)}" y2="${b.y.toFixed(1)}"><title>${esc(e.shared.join(", "))}</title></line>`;
    })
    .join("");
  // Nodes
  const nodes = graph.nodes
    .map((nd) => {
      const p = pos.get(nd.id)!;
      const tip = `${nd.name}\n${nd.capabilities.join(", ") || "(no capabilities)"}\nedges: ${graph.edges.filter((e) => e.from === nd.id || e.to === nd.id).length}`;
      const idShort = nd.id.replace(/^com\.opencapx\./, "");
      return `<g class="dep-node" data-id="${esc(nd.id)}"><circle cx="${p.x.toFixed(1)}" cy="${p.y.toFixed(1)}" r="22"><title>${esc(tip)}</title></circle><text x="${p.x.toFixed(1)}" y="${(p.y + 38).toFixed(1)}" text-anchor="middle" font-size="11" font-family="ui-monospace,monospace" fill="currentColor">${esc(idShort.slice(0, 22))}</text></g>`;
    })
    .join("");
  return `<svg class="dep-graph" viewBox="0 0 ${w} ${h}" width="100%" style="overflow:visible"><g class="dep-edges">${edges}</g><g class="dep-nodes">${nodes}</g></svg>`;
}

async function refreshDepGraph(): Promise<void> {
  const box = document.getElementById("dep-graph");
  const msg = document.getElementById("dep-msg");
  if (!box) return;
  try {
    const graph = await invoke<DependencyGraph>("list_plugin_dependency_graph");
    if (graph.nodes.length === 0) {
      box.innerHTML = `<p class="empty">${esc(t("depGraphEmpty"))}</p>`;
      if (msg) msg.textContent = "";
      return;
    }
    box.innerHTML = renderDepGraph(graph);
    if (msg) msg.textContent = `${graph.nodes.length} ${esc(t("depGraphNodes"))} · ${graph.edges.length} ${esc(t("depGraphEdges"))}`;
    // hover highlight: mouse enters a node -> highlight its edges + its neighbors
    box.querySelectorAll<SVGGElement>(".dep-node").forEach((g) => {
      const id = g.dataset.id;
      g.addEventListener("mouseenter", () => {
        box.querySelectorAll<SVGLineElement>(".dep-edge").forEach((line) => {
          const touches = line.dataset.from === id || line.dataset.to === id;
          line.classList.toggle("active", touches);
        });
        g.classList.add("active");
      });
      g.addEventListener("mouseleave", () => {
        box.querySelectorAll(".dep-edge.active").forEach((el) => el.classList.remove("active"));
        g.classList.remove("active");
      });
    });
  } catch (e) {
    if (msg) msg.textContent = `✗ ${String(e)}`;
  }
}

// Phase 42 — start / stop topological sort plan.
// layers[i] = the set of plugins started in parallel at step i; stop_order = layers reversed + each layer reversed.
interface LifecyclePlan {
  layers: string[][];
  stopOrder: string[];
  edges: DependencyEdge[];
}

interface StartAllResult {
  started: number;
  errors: string[];
}

function renderLifecyclePlan(plan: LifecyclePlan): string {
  if (plan.layers.every((l) => l.length === 0)) {
    return `<p class="empty">${esc(t("lifecycleEmpty"))}</p>`;
  }
  const layerHtml = plan.layers
    .map((layer, idx) => {
      const chips = layer
        .map((id) => `<span class="lifecycle-chip">${esc(id)}</span>`)
        .join(" ");
      const arrow =
        idx < plan.layers.length - 1
          ? `<span class="lifecycle-arrow">↓</span>`
          : "";
      return `<div class="lifecycle-layer"><div class="lifecycle-layer-label">${esc(t("lifecycleLayer"))} ${idx + 1}</div><div class="lifecycle-chips">${chips}</div>${arrow}</div>`;
    })
    .join("");
  const stopHtml = plan.stopOrder
    .map((id) => `<span class="lifecycle-chip stop">${esc(id)}</span>`)
    .join(" → ");
  return `<div class="lifecycle-start"><div class="lifecycle-section-label">${esc(t("lifecycleStartLabel"))}</div>${layerHtml}</div><div class="lifecycle-stop"><div class="lifecycle-section-label">${esc(t("lifecycleStopLabel"))}</div><div class="lifecycle-stop-order">${stopHtml}</div></div>`;
}

async function refreshLifecyclePlan(): Promise<void> {
  const box = document.getElementById("lifecycle-plan");
  const msg = document.getElementById("lifecycle-msg");
  if (!box) return;
  try {
    const plan = await invoke<LifecyclePlan>("get_plugin_lifecycle_plan");
    const totalPlugins = plan.layers.reduce((s, l) => s + l.length, 0);
    if (totalPlugins === 0) {
      box.innerHTML = `<p class="empty">${esc(t("lifecycleEmpty"))}</p>`;
      if (msg) msg.textContent = "";
      return;
    }
    box.innerHTML = renderLifecyclePlan(plan);
    if (msg)
      msg.textContent = `${totalPlugins} ${esc(t("lifecyclePlugins"))} · ${plan.layers.length} ${esc(t("lifecycleLayers"))}`;
  } catch (e) {
    if (msg) msg.textContent = `✗ ${String(e)}`;
  }
}

async function onStartAllPlugins(): Promise<void> {
  const msg = document.getElementById("lifecycle-msg");
  const btn = document.getElementById(
    "lifecycle-start-all",
  ) as HTMLButtonElement | null;
  if (!btn) return;
  if (!window.confirm(t("lifecycleStartConfirm"))) return;
  btn.disabled = true;
  try {
    const res = await invoke<StartAllResult>("start_all_plugins");
    if (msg) {
      if (res.errors.length === 0) {
        msg.textContent = `✓ ${res.started} ${esc(t("lifecycleStarted"))}`;
      } else {
        msg.textContent = `${res.started} ✓ · ${res.errors.length} ✗`;
      }
    }
    void refreshPlugins();
    void refreshLifecyclePlan();
  } catch (e) {
    if (msg) msg.textContent = `✗ ${String(e)}`;
  } finally {
    btn.disabled = false;
  }
}

async function onStopAllPlugins(): Promise<void> {
  const msg = document.getElementById("lifecycle-msg");
  const btn = document.getElementById(
    "lifecycle-stop-all",
  ) as HTMLButtonElement | null;
  if (!btn) return;
  if (!window.confirm(t("lifecycleStopConfirm"))) return;
  btn.disabled = true;
  try {
    const stopped = await invoke<number>("stop_all_plugins");
    if (msg) msg.textContent = `✓ ${stopped} ${esc(t("lifecycleStopped"))}`;
    void refreshPlugins();
    void refreshLifecyclePlan();
  } catch (e) {
    if (msg) msg.textContent = `✗ ${String(e)}`;
  } finally {
    btn.disabled = false;
  }
}
// Top denied = denied ranking aggregated globally by permission (denied desc, granted desc, name asc).
interface HeatmapCell {
  pluginId: string;
  permission: string;
  decision: string;
  highRisk: boolean;
}
interface TopPermission {
  permission: string;
  grantedCount: number;
  deniedCount: number;
  askCount: number;
  highRisk: boolean;
}
interface PermissionHeatmap {
  cells: HeatmapCell[];
  topDenied: TopPermission[];
}

function renderHeatmap(heatmap: PermissionHeatmap): { grid: string; top: string } {
  if (heatmap.cells.length === 0) {
    return {
      grid: `<p class="empty">${esc(t("heatmapEmpty"))}</p>`,
      top: "",
    };
  }
  // X axis = plugin (row), Y axis = permission (column); aggregated data
  const plugins = Array.from(new Set(heatmap.cells.map((c) => c.pluginId))).sort();
  const perms = Array.from(new Set(heatmap.cells.map((c) => c.permission))).sort();
  const cellMap = new Map<string, HeatmapCell>();
  for (const c of heatmap.cells) cellMap.set(`${c.pluginId}|${c.permission}`, c);

  const headerCells = perms.map((p) => {
    const isHigh = heatmap.topDenied.find((t) => t.permission === p)?.highRisk ?? false;
    const label = p.replace(/^.*\./, ""); // shorten image.read -> read
    const cls = isHigh ? "heatmap-col high-risk" : "heatmap-col";
    return `<th class="${cls}" title="${esc(p)}">${esc(label)}</th>`;
  }).join("");

  const rows = plugins.map((plugin) => {
    const idShort = plugin.replace(/^com\.opencapx\./, "");
    const cells = perms.map((perm) => {
      const c = cellMap.get(`${plugin}|${perm}`);
      if (!c) return `<td class="heatmap-cell empty" title="${esc(plugin)} · ${esc(perm)}">·</td>`;
      const cls = `heatmap-cell ${c.decision}${c.highRisk ? " high-risk" : ""}`;
      const tip = `${plugin} · ${perm} · ${c.decision}${c.highRisk ? " (high-risk)" : ""}`;
      const icon = c.decision === "granted" ? "✓" : c.decision === "denied" ? "✗" : "?";
      return `<td class="${cls}" title="${esc(tip)}">${icon}</td>`;
    }).join("");
    return `<tr><th class="heatmap-row" title="${esc(plugin)}">${esc(idShort.slice(0, 22))}</th>${cells}</tr>`;
  }).join("");

  const grid = `<table class="heatmap"><thead><tr><th></th>${headerCells}</tr></thead><tbody>${rows}</tbody></table>`;

  const top = heatmap.topDenied.length === 0
    ? `<p class="muted">${esc(t("heatmapNoTop"))}</p>`
    : `<ul class="heatmap-top">${heatmap.topDenied
        .map((t) => {
          const cls = t.highRisk ? "heatmap-top-row high-risk" : "heatmap-top-row";
          const pieces: string[] = [];
          if (t.grantedCount > 0) pieces.push(`<span class="granted">${t.grantedCount} ✓</span>`);
          if (t.deniedCount > 0) pieces.push(`<span class="denied">${t.deniedCount} ✗</span>`);
          if (t.askCount > 0) pieces.push(`<span class="ask">${t.askCount} ?</span>`);
          return `<li class="${cls}"><span class="heatmap-perm">${esc(t.permission)}</span>${pieces.join(" ")}</li>`;
        })
        .join("")}</ul>`;

  return { grid, top };
}

async function refreshHeatmap(): Promise<void> {
  const gridBox = document.getElementById("heatmap-grid");
  const topBox = document.getElementById("heatmap-top");
  const msg = document.getElementById("heatmap-msg");
  if (!gridBox || !topBox) return;
  try {
    const data = await invoke<PermissionHeatmap>("list_permission_heatmap");
    const { grid, top } = renderHeatmap(data);
    gridBox.innerHTML = grid;
    topBox.innerHTML = top;
    if (msg) msg.textContent = `${data.cells.length} ${esc(t("heatmapCells"))}`;
  } catch (e) {
    if (msg) msg.textContent = `✗ ${String(e)}`;
  }
}

async function refreshPlugins(): Promise<void> {
  const box = document.getElementById("plugin-list");
  let plugins: PluginStatus[] = [];
  let perms: PluginPermissions[] = [];
  try {
    plugins = await invoke<PluginStatus[]>("list_plugins");
    perms = await invoke<PluginPermissions[]>("list_permissions");
  } catch {
    /* commands unavailable */
  }
  // Sidebar 'Plugin Settings' section: display names come from this list_plugins call; then refresh that section along with install/uninstall/update
  // (refreshPluginConfig updates the cache and entries, even when not currently on a plugin-related tab).
  setPluginNameById(new Map(plugins.map((p) => [p.id, p.name])));
  void refreshPluginConfig().catch(() => undefined);
  if (!box) return;
  // Check for updates (started in parallel; on failure treat as no update)
  pluginUpdates = {};
  try {
    const updates = await invoke<PluginUpdateInfo[]>("check_plugin_updates");
    for (const u of updates) pluginUpdates[u.id] = u;
  } catch {
    /* marketplace not configured */
  }
  if (plugins.length === 0) {
    box.innerHTML = `<p class="empty">${esc(t("pluginsNone"))}</p>`;
    return;
  }
  const permByPlugin = new Map(perms.map((p) => [p.plugin_id, p]));
  // Detail mode: the list exits and only the selected plugin + README render. If the selected plugin no longer exists (uninstalled) -> fall back to the list.
  let detail = plugins.find((p) => p.id === pluginDetailId) ?? null;
  if (pluginDetailId && !detail) pluginDetailId = null;
  // Single-plugin card HTML: in list mode the title row is clickable to enter details; detail mode adds a row of author/license/homepage meta.
  const pluginCardHtml = (p: PluginStatus, detail: boolean): string => {
      const running = p.status === "running";
      const update = pluginUpdates[p.id];
      const updateBadge = update
        ? `<span class="plugin-update-badge channel-${esc(update.channel ?? "stable")}" title="${esc(t("pluginUpdateAvailable"))}">↑ v${esc(update.latestVersion)}</span>`
          : "";
      const updateBtn = update
        ? `<button class="btn ghost" data-plugin-update="${esc(p.id)}" type="button">${esc(t("pluginUpdate"))}</button>`
        : "";
      // Local-file update: packages the channel/marketplace can't see (installed manually from .ocplugin) also get a non-destructive update entry,
      // right next to the channel update button, read as its 'local counterpart'.
      const updateFileBtn = `<button class="btn ghost" data-plugin-update-file="${esc(p.id)}" type="button">${esc(t("pluginUpdateFromFile"))}</button>`;
      const probeBadge = p.probeStatus
        ? `<span class="probe-status probe-${esc(p.probeStatus)}" title="${esc(p.probeAt ? new Date(p.probeAt * 1000).toLocaleString() : "")}">${esc(probeStatusLabel(p.probeStatus))}</span>`
        : "";
      const channelSel = `<select class="channel-select" data-plugin-channel="${esc(p.id)}" title="${esc(t("channelSelectHint"))}">${CHANNEL_OPTIONS.map(
        (o) => `<option value="${esc(o.key)}"${(p.channel ?? "stable") === o.key ? " selected" : ""}>${esc(t(o.i18nKey as never))}</option>`,
      ).join("")}</select>`;
      const channelBadge = channelBadgeHtml(p.channel);
      const missingDepsList = p.missingDependencies ?? [];
      const missingDeps = missingDepsList.length > 0
        ? `<div class="plugin-desc warn">${esc(t("pluginMissingDeps"))}: ${missingDepsList.map((d) => `${esc(d.id)} (${esc(d.requirement)})`).join(", ")}</div>`
        : "";
      const revokedBanner = p.revokedKey
        ? `<div class="plugin-desc warn">${esc(t("pluginRevoked"))} (${esc(p.revokedKey)}) <button class="btn ghost" data-plugin-reopen="${esc(p.id)}" type="button">${esc(t("pluginReopen"))}</button></div>`
        : "";
      const head = `<div class="plugin-card">
        <div class="plugin-title-line"${detail ? "" : ` data-plugin-open="${esc(p.id)}"`}><span class="plugin-name">${esc(p.name)}</span><span class="plugin-status plugin-status-${esc(p.status)}">${esc(p.status)}</span>${updateBadge}${probeBadge}${channelBadge}${p.sandboxDeclared ? `<span class="cap-badge">${esc(t("pluginSandboxBadge"))}</span>` : ""}${detail ? `<span class="plugin-detail-open-hint">›</span>` : ""}</div>
        <div class="plugin-sub-line"><span class="plugin-id">${esc(p.id)}</span><span class="plugin-sub-sep">·</span><span class="plugin-ver">v${esc(p.version)}</span>${p.capabilities.length ? `<span class="plugin-caps">${p.capabilities.map((c) => `<span class="cap-badge">${esc(c)}</span>`).join("")}</span>` : ""}</div>
        ${detail && (p.author || p.license || p.homepage) ? `<div class="plugin-meta-line">${esc([p.author, p.license, p.homepage].filter(Boolean).join(" · "))}</div>` : ""}
        <div class="plugin-actions">${detail ? `<button class="btn ghost" data-plugin-cfg="${esc(p.id)}" type="button">${esc(t("pluginDetailOpenCfg"))}</button>` : ""}${updateBtn}${updateFileBtn}<button class="btn ghost" data-plugin-toggle="${esc(p.id)}" type="button">${esc(running ? t("pluginDisable") : t("pluginEnable"))}</button><button class="btn ghost" data-plugin-probe="${esc(p.id)}" data-plugin-probe-name="${esc(p.name)}" type="button">${esc(t("probeBtn"))}</button><button class="btn ghost" data-plugin-lifecycle="${esc(p.id)}" type="button">${esc(t("lifecycleBtn"))}</button><button class="btn ghost" data-plugin-trace="${esc(p.id)}" type="button">${esc(t("traceBtn"))}</button><button class="btn ghost" data-plugin-health="${esc(p.id)}" data-plugin-health-name="${esc(p.name)}" type="button">${esc(t("healthBtn"))}</button><button class="btn ghost danger" data-plugin-uninstall="${esc(p.id)}" type="button">${esc(t("pluginUninstall"))}</button></div>
        <div class="plugin-config-row"><div class="plugin-channel-row"><span class="setting-hint">${esc(t("channelRowLabel"))}</span>${channelSel}</div><label class="plugin-autoreload"><input type="checkbox" data-plugin-autoreload="${esc(p.id)}"${p.autoReload ? " checked" : ""}/><span>${esc(t("pluginAutoReload"))}</span></label></div>
        ${missingDeps}${revokedBanner}
        ${p.description ? `<div class="plugin-desc">${esc(p.description)}</div>` : `<div class="plugin-desc muted">${esc(t("pluginNoDescription"))}</div>`}
        ${p.path ? `<div class="plugin-path" title="${esc(p.path)}">${esc(p.path)}</div>` : ""}`;
      const entries = permByPlugin.get(p.id)?.permissions ?? [];
      const permRows = entries
        .map((e) => {
          // docs/permission-domains.md §4.3 enforcement point 3: declared derived permissions offer only ask/denied
          // (Core's set_decision also rejects granted; this just avoids a pointless click)
          const noAlways = e.high_risk || e.declared;
          const opts = noAlways && e.decision !== "granted"
            ? [["ask", t("permAsk")], ["denied", t("permDenied")]]
            : [["granted", t("permGranted")], ["ask", t("permAsk")], ["denied", t("permDenied")]];
          const sel = `<select data-perm-plugin="${esc(p.id)}" data-perm="${esc(e.permission)}">${opts
            .map(([v, l]) => `<option value="${esc(v)}"${v === e.decision ? " selected" : ""}>${esc(l)}</option>`)
            .join("")}</select>`;
          const badge = (e.high_risk ? ` <span class="setting-hint">${esc(t("permHighRisk"))}</span>` : "")
            + (e.declared ? ` <span class="setting-hint">${esc(t("permDeclared"))}</span>` : "");
          const reset = `<button class="btn ghost" data-perm-reset="${esc(e.permission)}" data-perm-def="${esc(e.default)}" data-perm-plugin="${esc(p.id)}" type="button">${esc(t("permReset"))}</button>`;
          return `<div class="setting-row"><div class="setting-info"><span class="setting-label">${esc(e.permission)}${badge}</span></div><div class="perm-controls">${sel}${reset}</div></div>`;
        })
        .join("");
      return `${head}${permRows ? `<div class="settings-list plugin-perms">${permRows}</div>` : ""}</div>`;
  };
  box.innerHTML = detail
    ? `<button class="btn ghost plugin-detail-back" data-plugin-back type="button">‹ ${esc(t("pluginDetailBack"))}</button>`
      + pluginCardHtml(detail, true)
      + `<div class="plugin-readme"><div class="plugin-readme-title">${esc(t("pluginReadmeTitle"))}</div><div class="plugin-readme-body" id="plugin-readme-body">${esc(t("pluginReadmeLoading"))}</div></div>`
    : plugins.map((p) => pluginCardHtml(p, false)).join("");
  box.querySelectorAll("button[data-plugin-toggle]").forEach((b) => {
    b.addEventListener("click", async () => {
      await invoke("toggle_plugin", { id: (b as HTMLElement).dataset.pluginToggle });
      await refreshPlugins();
    });
  });
  box.querySelectorAll("button[data-plugin-update]").forEach((b) => {
    b.addEventListener("click", async () => {
      const el = b as HTMLButtonElement;
      const id = el.dataset.pluginUpdate ?? "";
      el.disabled = true;
      el.textContent = t("pluginUpdateChecking");
      try {
        // F7 — unified preview: download + verify + diff / key-change info -> one dialog -> install after confirmation.
        const res = await invoke<UpdatePreviewResponse>("preview_update", { id });
        const ok = await showInstallPreviewDialog(res.preview, {
          mode: "update",
          publisherChange: res.publisherChange ?? null,
        });
        if (!ok) {
          el.disabled = false;
          el.textContent = t("pluginUpdate");
          return;
        }
        const softWarn =
          res.preview.signature.status === "unsigned" ||
          res.preview.signature.status === "unknown-key";
        await invoke("install_ocplugin", {
          path: res.archivePath,
          confirmUnsigned: softWarn,
          confirmKeyChange: res.publisherChange != null,
        });
      } catch (err) {
        const m = document.getElementById("plugin-install-msg");
        if (m) m.textContent = `${t("pluginUpdateFailed")}: ${(err as Error).message ?? err}`;
        el.disabled = false;
        el.textContent = t("pluginUpdate");
        return;
      }
      await refreshPlugins();
    });
  });
  // Local-file update: the same non-destructive install path as 'install' (no uninstall), it just verifies the package id first, then previews and confirms.
  box.querySelectorAll("button[data-plugin-update-file]").forEach((b) => {
    b.addEventListener("click", async () => {
      const el = b as HTMLElement;
      const id = el.dataset.pluginUpdateFile ?? "";
      const msg = document.getElementById("plugin-install-msg");
      try {
        const picked = await open({
          multiple: false,
          directory: false,
          filters: [{ name: "OpenCapX Plugin", extensions: ["ocplugin"] }],
        });
        if (!picked) return; // cancel: silent
        const path = typeof picked === "string" ? picked : picked;
        const installed = await installPreviewedPlugin(path, {
          mode: "update",
          // The package id must equal the target plugin id — never let one plugin's package overwrite another
          guard: (preview) =>
            preview.id === id
              ? null
              : t("pluginUpdateFileIdMismatch").replace("{want}", id).replace("{got}", preview.id),
        });
        if (!installed) return; // cancel in the preview dialog: silent
        if (msg) msg.textContent = `✓ ${installed}`;
      } catch (e) {
        if (msg) msg.textContent = `✗ ${String(e)}`;
      }
      await refreshPlugins();
    });
  });
  box.querySelectorAll("button[data-plugin-reopen]").forEach((b) => {
    b.addEventListener("click", async () => {
      const id = (b as HTMLElement).dataset.pluginReopen ?? "";
      try {
        await invoke("reopen_plugin", { id });
      } catch (err) {
        const m = document.getElementById("plugin-install-msg");
        if (m) m.textContent = `${t("pluginReopenFailed")}: ${(err as Error).message ?? err}`;
      }
      await refreshPlugins();
    });
  });
  box.querySelectorAll("button[data-plugin-lifecycle]").forEach((b) => {
    b.addEventListener("click", () => {
      const id = (b as HTMLElement).dataset.pluginLifecycle ?? "";
      const name = (b as HTMLElement).dataset.pluginLifecycleName ?? id;
      void showLifecycleDialog(id, name);
    });
  });
  box.querySelectorAll("button[data-plugin-trace]").forEach((b) => {
    b.addEventListener("click", () => {
      const el = b as HTMLElement;
      const id = el.dataset.pluginTrace ?? "";
      const name = el.dataset.pluginTraceName ?? id;
      void showTraceDialog(id, name);
    });
  });
  box.querySelectorAll("button[data-plugin-probe]").forEach((b) => {
    b.addEventListener("click", () => {
      const el = b as HTMLElement;
      const id = el.dataset.pluginProbe ?? "";
      const name = el.dataset.pluginProbeName ?? id;
      void showProbeDialog(id, name, refreshPlugins);
    });
  });
  box.querySelectorAll("button[data-plugin-health]").forEach((b) => {
    b.addEventListener("click", () => {
      const el = b as HTMLElement;
      const id = el.dataset.pluginHealth ?? "";
      const name = el.dataset.pluginHealthName ?? id;
      void showHealthDialog(id, name, refreshPlugins);
    });
  });
  box.querySelectorAll("button[data-plugin-uninstall]").forEach((b) => {
    b.addEventListener("click", async () => {
      const el = b as HTMLElement;
      const id = el.dataset.pluginUninstall ?? "";
      let preview: UninstallPreview;
      try {
        preview = await invoke<UninstallPreview>("preview_uninstall_plugin", { id });
      } catch (err) {
        // Backend can't read it -> degrade to a simple confirmation
        if (!window.confirm(`${t("pluginUninstallConfirm")}\n\n${id}`)) return;
        try {
          await invoke("uninstall_plugin", { id });
        } catch (e) {
          const m = document.getElementById("plugin-install-msg");
          if (m) m.textContent = `${t("pluginUninstallFailed")}: ${(e as Error).message ?? e}`;
        }
        await refreshPlugins();
        return;
      }
      const ok = await showUninstallPreviewDialog(preview);
      if (!ok) return;
      try {
        await invoke("uninstall_plugin", { id });
      } catch (err) {
        const m = document.getElementById("plugin-install-msg");
        if (m) m.textContent = `${t("pluginUninstallFailed")}: ${(err as Error).message ?? err}`;
        return;
      }
      await refreshPlugins();
    });
  });
  // Auto-reload toggle: once on, a change to the manifest mtime automatically stops+starts.
  // The poller singleton runs in core (spawn_auto_reload_poller during setup); this just
  // flips the per-manager set + SQL flag. No need to refresh the whole list, since the state is on the input itself.
  box.querySelectorAll("input[data-plugin-autoreload]").forEach((c) => {
    c.addEventListener("change", async () => {
      const el = c as HTMLInputElement;
      const id = el.dataset.pluginAutoreload ?? "";
      try {
        await invoke("set_plugin_auto_reload", { id, on: el.checked });
      } catch (err) {
        el.checked = !el.checked; // failure: roll back the UI
        const m = document.getElementById("plugin-install-msg");
        if (m) m.textContent = `✗ ${String(err)}`;
      }
    });
  });
  box.querySelectorAll("select[data-plugin-channel]").forEach((s) => {
    s.addEventListener("change", async () => {
      const el = s as HTMLSelectElement;
      const id = el.dataset.pluginChannel ?? "";
      try {
        await invoke("set_plugin_channel", { id, channel: el.value });
      } catch (err) {
        const m = document.getElementById("plugin-install-msg");
        if (m) m.textContent = `${t("channelSetFailed")}: ${(err as Error).message ?? err}`;
      }
      await refreshPlugins();
    });
  });
  box.querySelectorAll("select[data-perm]").forEach((s) => {
    s.addEventListener("change", async () => {
      const el = s as HTMLSelectElement;
      const pluginId = el.dataset.permPlugin ?? "";
      const permission = el.dataset.perm ?? "";
      try {
        await invoke("set_permission", { pluginId, permission, decision: el.value });
      } catch {
        /* High-risk rejected changes etc., echoed back on refresh */
      }
      await refreshPlugins();
    });
  });
  box.querySelectorAll("button[data-perm-reset]").forEach((b) => {
    b.addEventListener("click", async () => {
      const el = b as HTMLElement;
      try {
        await invoke("set_permission", {
          pluginId: el.dataset.permPlugin,
          permission: el.dataset.permReset,
          decision: el.dataset.permDef,
        });
      } catch {
        /* ignore */
      }
      await refreshPlugins();
    });
  });
  // ── list<->detail navigation: click the list title row to enter; Back returns; Open config jumps to the config tab filtered by plugin id ──
  box.querySelectorAll("[data-plugin-open]").forEach((el) => {
    el.addEventListener("click", () => {
      pluginDetailId = (el as HTMLElement).dataset.pluginOpen ?? null;
      void refreshPlugins();
    });
  });
  box.querySelectorAll("[data-plugin-back]").forEach((b) => {
    b.addEventListener("click", () => {
      pluginDetailId = null;
      void refreshPlugins();
    });
  });
  box.querySelectorAll("[data-plugin-cfg]").forEach((b) => {
    b.addEventListener("click", () => {
      const id = (b as HTMLElement).dataset.pluginCfg ?? "";
      pluginDetailId = null;
      openPluginSettingsPage(id, "plugins");
    });
  });
  if (detail) {
    void renderPluginReadme(detail.id);
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

/// Detail-page README: three states (loading -> present/absent). Rendered with marked + sanitized with DOMPurify before entering
/// innerHTML — the README is plugin-author content, treated as untrusted. If the detail has switched away by the time it returns (back to
/// the list / another plugin / a re-render), it is discarded, not written.
async function renderPluginReadme(id: string): Promise<void> {
  let md: string | null = null;
  try {
    md = await invoke<string | null>("read_plugin_readme", { id });
  } catch {
    /* Command unavailable: treat as no README */
  }
  const target = document.getElementById("plugin-readme-body");
  if (!target || pluginDetailId !== id) return;
  if (!md) {
    target.textContent = t("pluginReadmeMissing");
    return;
  }
  const html = marked.parse(md, { async: false });
  target.innerHTML = DOMPurify.sanitize(typeof html === "string" ? html : "");
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

/// Plugin detail (clicking a list item enters detail): the plugin id of the current detail page, null = list mode.
/// Kept at module level — operations inside the detail (toggle/update/permissions) re-render the whole card, so the selected state must not be lost.
let pluginDetailId: string | null = null;

startThemeListener();

void getCurrentWindow()
  .onFocusChanged(({ payload: focused }) => {
    setWindowFocused(focused);
  })
  .catch(() => undefined);

registerLegacyCleanup(() => {
  stopAuditStream();
  stopLogsStream();
  stopMetricsStream();
  stopWorkspaceListener();
});
registerLegacyRenderer(renderLegacyTab);
registerTab({ id: "backup", render: renderBackup });
registerTab({ id: "notify", render: renderNotify });
registerTab({ id: "profiles", render: renderProfilesTab });
registerTab({ id: "stats", render: renderStats });
registerTab({ id: "sla", render: renderSla });

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

function stopMetricsStream(): void {
  metricsUnlistenSampled?.();
  metricsUnlistenSampled = null;
  metricsUnlistenExceeded?.();
  metricsUnlistenExceeded = null;
}

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



