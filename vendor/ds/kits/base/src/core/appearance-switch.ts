import { applyAppearance, type AppearanceAxis, type AppearanceOption, type AppearanceStorage } from "../appearance-switch.behavior";

export interface AppearanceSwitchContract {
  /** Visible accessible control name. */
  label: string;
  /** The single independent axis this control changes. */
  axis: AppearanceAxis;
  /** Caller-owned values; the component invents no Theme, scheme, or stop. */
  options: readonly AppearanceOption[];
  /** Current axis value; caller-owned in controlled React use. */
  value?: string;
  /** Initial uncontrolled value, restored by native form reset. */
  defaultValue?: string;
  /** Root receiving the axis attribute. Defaults to the document root. */
  root?: HTMLElement;
  /** Persistence seam. Defaults to browser localStorage when available. */
  storage?: AppearanceStorage;
  /** Stable persistence key. Defaults to one key per independent axis. */
  persistKey?: string;
}

/** Only enabled caller-owned options can change an appearance axis. */
export function appearanceValue(options: readonly AppearanceOption[], ...candidates: (string | null | undefined)[]): string | undefined {
  for (const value of candidates) if (options.some(option => !option.disabled && option.value === value)) return value!;
  return options.find(option => !option.disabled)?.value;
}
export function appearancePersistence(storage?: AppearanceStorage): AppearanceStorage | undefined {
  if (storage) return storage;
  try { return globalThis.localStorage; } catch { return undefined; }
}
export function readAppearance(storage: AppearanceStorage | undefined, key: string): string | undefined {
  try { return storage?.getItem(key) ?? undefined; } catch { return undefined; }
}
/** Persistence is optional; blocked storage cannot prevent an appearance change. */
export function commitAppearance(root: HTMLElement | undefined, axis: AppearanceAxis, value: string | undefined, storage: AppearanceStorage | undefined, key: string): void {
  if (value === undefined) return;
  if (root) applyAppearance(root, axis, value);
  try { storage?.setItem(key, value); } catch { /* Browser policy or quota: retain the usable in-memory choice. */ }
}
