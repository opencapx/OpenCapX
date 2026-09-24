// Unit tests for bubble.ts formatting helpers and the i18n fallback chain.
// phraseFor goes through t(), so these also pin the locale pack contract:
// every theme/state pair must resolve to a non-empty string in every pack.

import { describe, expect, it } from "vitest";
import { BUBBLE_THEMES, elapsedText, phraseFor } from "./bubble";
import { availableLocales, getLocale, setLocale, t } from "./i18n";
import type { AgentState } from "./shared";

describe("elapsedText", () => {
  it("formats seconds under a minute", () => {
    expect(elapsedText(0, 0)).toBe("0s");
    expect(elapsedText(0, 59_999)).toBe("59s");
    expect(elapsedText(59, 59_999)).toBe("0s"); // same whole second
  });

  it("formats minutes under an hour", () => {
    expect(elapsedText(0, 60_000)).toBe("1m");
    expect(elapsedText(0, 59 * 60_000 + 59_000)).toBe("59m");
  });

  it("formats hours with leftover minutes", () => {
    expect(elapsedText(0, 3_600_000)).toBe("1h 0m");
    expect(elapsedText(0, 3 * 3_600_000 + 7 * 60_000)).toBe("3h 7m");
  });

  it("never returns negative for future timestamps", () => {
    expect(elapsedText(10_000, 0)).toBe("0s");
  });
});

describe("phraseFor × locale packs", () => {
  const states: AgentState[] = ["working", "waiting", "done", "idle"];

  it("resolves a non-empty phrase for every theme/state in every pack", () => {
    for (const { code } of availableLocales()) {
      setLocale(code);
      for (const theme of BUBBLE_THEMES) {
        for (const state of states) {
          const phrase = phraseFor(theme, state);
          expect(phrase, `${code}/${theme}/${state}`).toBeTruthy();
        }
      }
    }
    setLocale("en");
  });
});

describe("i18n fallbacks", () => {
  it("falls back to English for an unknown locale", () => {
    setLocale("xx-YY");
    expect(getLocale()).toBe("en");
    expect(t("tabGeneral")).not.toBe("");
  });

  it("pins English first in the menu regardless of self-name sorting", () => {
    const codes = availableLocales().map((l) => l.code);
    expect(codes[0]).toBe("en");
  });

  it("returns the key itself when a pack misses it (defensive; i18n:check pins parity)", () => {
    setLocale("en");
    expect(t("definitely.not.a.key" as never)).toBe("definitely.not.a.key");
  });
});
