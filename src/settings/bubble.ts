import { t, type I18nKey } from "../i18n";
import {
  BUBBLE_DENSITIES,
  BUBBLE_POSITIONS,
  BUBBLE_THEMES,
  type BubblePos,
  type BubbleTheme,
} from "../bubble";
import { DEFAULTS, clampInt, esc, escAttr, getSettings, group, render, row, save, segmented, setSettings, toggle } from "./shared";

export function renderBubbleSettings(body: HTMLElement): void {
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
    return `<button class="bubble-pos${on ? " active" : ""}" data-pos="${p}" type="button" aria-pressed="${on}" title="${escAttr(t(POS_I18N[p]))}">
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
}
