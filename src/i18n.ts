import en from "./locales/en.json";

/**
 * Internationalization.
 *
 * Conventions:
 * - `src/locales/en.json` is the **single base**. All keys are anchored to it,
 *   and other language packs must be neither a superset nor a subset — the key sets must match exactly (`npm run i18n:check` validates).
 * - Adding a language = drop in a `src/locales/<code>.json` (same key set as en) + write
 *   the language's self-name (the name shown in the menu) in `src/locales/locales.json`.
 *   `npm run i18n:scaffold -- <code> --name "<self-name>"` does both steps at once.
 * - Language packs are **auto-discovered** (see below), so adding a language needs no change to this file.
 * - Native-side strings (tray/notifications) live in Rust's `core::i18n`, holding only the few that cannot read the WebView.
 */

/** Language code, e.g. `en` / `zh-Hans`. Validated at runtime (falls back to English when not found). */
export type Locale = string;

/** Message key: anchored to the English base; a wrong key fails at compile time. */
export type I18nKey = keyof typeof en;

type Pack = Record<string, string>;

/** Auto-discover src/locales/*.json — adding a language needs no code change. */
const modules = import.meta.glob("./locales/*.json", { eager: true }) as Record<
  string,
  { default?: Pack }
>;

const packs: Record<string, Pack> = {};
for (const [path, mod] of Object.entries(modules)) {
  const code = path.slice("./locales/".length, -".json".length);
  if (code === "locales") continue; // this name table is not a language pack
  if (mod.default) packs[code] = mod.default;
}

/** Language self-names (en / Simplified Chinese / Tiếng Việt…), from locales.json. */
const names: Record<string, string> = (modules["./locales/locales.json"]?.default ?? {}) as Record<
  string,
  string
>;

const label = (code: string): string => names[code] ?? code;

/** Available languages: English is pinned first, the rest sorted by self-name. */
export function availableLocales(): Array<{ code: Locale; name: string }> {
  return Object.keys(packs)
    .sort((a, b) => {
      if (a === "en") return -1;
      if (b === "en") return 1;
      return label(a).localeCompare(label(b));
    })
    .map((code) => ({ code, name: label(code) }));
}

let current: Locale = "en";

export function setLocale(l: Locale): void {
  current = packs[l] ? l : "en";
}

export function getLocale(): Locale {
  return current;
}

export function t(key: I18nKey): string {
  return packs[current]?.[key] ?? en[key] ?? key;
}
