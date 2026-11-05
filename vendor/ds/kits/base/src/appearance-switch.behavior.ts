/** The independently selectable appearance axes established by ADRs 0003 and 0008. */
export const APPEARANCE_AXES = ["theme", "color-scheme", "density"] as const;
export type AppearanceAxis = (typeof APPEARANCE_AXES)[number];

/** One caller-owned value on an appearance axis. */
export interface AppearanceOption {
  value: string;
  label: string;
  disabled?: boolean;
}

/** Minimal persistence surface, injectable without depending on browser globals. */
export interface AppearanceStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

export function appearanceAttribute(axis: AppearanceAxis): `data-${AppearanceAxis}` {
  return `data-${axis}`;
}

export function appearanceStorageKey(axis: AppearanceAxis): string {
  return `reddb:appearance:${axis}`;
}

/** Apply one value without touching either of the other independent axes. */
export function applyAppearance(root: HTMLElement, axis: AppearanceAxis, value: string): void {
  root.setAttribute(appearanceAttribute(axis), value);
}
