export interface HotkeyAction {
  kind: "builtin" | "plugin";
  // builtin
  action?: "toggle-pet" | "open-settings" | "open-palette" | "quit";
  // plugin
  plugin_id?: string;
  capability?: string;
}

export interface PaletteEntry {
  id: string;
  title: string;
  kind: "builtin" | "plugin";
  plugin_id: string | null;
  capability: string | null;
}

export interface PluginConfigRow {
  id: string;
  config: Record<string, unknown>;
}

/// Plugin text: a plain string, or a locale -> text mapping (same model as the Rust side).
export type LocalizedText = string | Record<string, string>;

/** M7/F8 — plugin settings[] declaration (aligned with Rust SettingDecl) */
export interface SettingDecl {
  key: string;
  type: string;
  label?: LocalizedText;
  description?: LocalizedText;
  default?: unknown;
  /// A bare string (display = value) or {value, label} — label is localized for display only; storage/predicates use value.
  options?: Array<string | { value: string; label?: LocalizedText }>;
  /// Data-driven predicate: when false the whole row is not rendered
  visible?: Cond;
  /// Data-driven predicate: when true the control is disabled (the row remains, so the reason it is disabled is visible)
  disabled?: Cond;
  /// Validation rules before writing to disk (data, not closures)
  validate?: ValidateRule[];
  /// Consecutive declarations with the same section share a heading (comparing raw values, not resolved text)
  section?: LocalizedText;
  /// Only for path: pick a file or a directory (default is still directory, preserving existing behavior)
  pick?: "file" | "directory";
  /// Only for number / slider
  min?: number;
  max?: number;
  step?: number;
  /// Search keywords (used for search only, never displayed)
  aliases?: string[];
  /// Display order: smaller first; those without it fall back to declaration order (stable sort, affects the form only).
  order?: number;
  /// Deprecation note (localized): the control stays usable; the row carries a 'Deprecated' marker and reason.
  deprecated?: LocalizedText;
}

export interface PluginSettingsView {
  settings: SettingDecl[];
  values: Record<string, unknown>;
  secretsSet: string[];
}

/// Predicates are data (cross-process: Python plugin -> Rust -> this UI), not closures; they depend only on stored values,
/// so the host can recompute after any write — no manual sync like update()/refresh.
export type Cond =
  | { op: "equals"; key: string; value: unknown }
  | { op: "notEquals"; key: string; value: unknown }
  | { op: "in"; key: string; values: unknown[] }
  | { op: "isSet"; key: string; value: true }
  | { op: "all"; conds: Cond[] }
  | { op: "any"; conds: Cond[] }
  | { op: "not"; cond: Cond };

/// Validation rules (also data).
export type ValidateRule =
  | { type: "required"; message?: LocalizedText }
  | { type: "minLength"; value: number; message?: LocalizedText }
  | { type: "maxLength"; value: number; message?: LocalizedText }
  | { type: "min"; value: number; message?: LocalizedText }
  | { type: "max"; value: number; message?: LocalizedText }
  | { type: "pattern"; regex: string; message?: LocalizedText };
