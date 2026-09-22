import type { AgentState } from "./shared";

export type SoundKind = "done" | "waiting";

const ENABLE_KEY = "opencapx.sound.enabled";
const CUSTOM_KEY = "opencapx.sound.custom";

function enabled(kind: SoundKind): boolean {
  try {
    const raw = localStorage.getItem(`${ENABLE_KEY}.${kind}`);
    return raw === null ? true : raw === "1";
  } catch {
    return true;
  }
}

export function setSoundEnabled(kind: SoundKind, on: boolean): void {
  try {
    localStorage.setItem(`${ENABLE_KEY}.${kind}`, on ? "1" : "0");
  } catch {
    /* ignore */
  }
}

export function isSoundEnabled(kind: SoundKind): boolean {
  return enabled(kind);
}

/** Store a user-uploaded sound (data URL) for a kind. Pass null to reset. */
export function setCustomSound(kind: SoundKind, dataUrl: string | null): void {
  try {
    if (dataUrl) localStorage.setItem(`${CUSTOM_KEY}.${kind}`, dataUrl);
    else localStorage.removeItem(`${CUSTOM_KEY}.${kind}`);
  } catch {
    /* ignore */
  }
}

function playDataUrl(dataUrl: string): void {
  const audio = new Audio(dataUrl);
  void audio.play().catch(() => undefined);
}

function playTone(kind: SoundKind): void {
  const ctx = new AudioContext();
  const notes =
    kind === "done" ? [523.25, 783.99] : [659.25, 659.25, 880];
  notes.forEach((freq, i) => {
    const osc = ctx.createOscillator();
    const gain = ctx.createGain();
    osc.type = "sine";
    osc.frequency.value = freq;
    const t = ctx.currentTime + i * 0.12;
    gain.gain.setValueAtTime(0.0001, t);
    gain.gain.exponentialRampToValueAtTime(0.3, t + 0.02);
    gain.gain.exponentialRampToValueAtTime(0.0001, t + 0.11);
    osc.connect(gain).connect(ctx.destination);
    osc.start(t);
    osc.stop(t + 0.12);
  });
  window.setTimeout(() => void ctx.close(), notes.length * 120 + 100);
}

/** Play the sound for a transitioned-to state, honoring toggles + uploads. */
export function notifySound(kind: SoundKind): void {
  if (!enabled(kind)) return;
  try {
    const custom = localStorage.getItem(`${CUSTOM_KEY}.${kind}`);
    if (custom) {
      playDataUrl(custom);
      return;
    }
  } catch {
    /* fall through to default tone */
  }
  try {
    playTone(kind);
  } catch {
    /* audio unavailable (e.g. no user gesture yet) */
  }
}

/** Call on every agent event with its previous state; plays on new waiting/done. */
export function soundForTransition(
  prev: AgentState | undefined,
  next: AgentState,
): void {
  if (prev !== next && (next === "waiting" || next === "done")) {
    notifySound(next);
  }
}
