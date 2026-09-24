import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { t, type I18nKey } from "../i18n";
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
} from "../petpack";
import { renderModelSnapshot } from "../pet3d";
import { MOOD_ROWS } from "../sprite";
import {
  esc,
  getSettings,
  group,
  row,
  save,
  setSettings,
  toggle,
} from "./shared";

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

export function renderPet(body: HTMLElement): void {
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
}
