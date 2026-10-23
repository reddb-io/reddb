import type { ToggleGroupOption } from "../toggle-group.variants";

export interface ToggleGroupContract {
  /** Visible group name, rendered as the native legend. */
  legend: string;
  /** Caller-owned choices; exactly one enabled option is pressed when available. */
  options: readonly ToggleGroupOption[];
  /** Selected option; an absent or unavailable value resolves to the first enabled option. */
  value?: string;
  /** Initial uncontrolled value, restored by native form reset. */
  defaultValue?: string;
  /** Optional native form name for the selected value. */
  name?: string;
  /** Disables the whole group, so no option can be chosen. */
  disabled?: boolean;
  /** Supporting text announced with every option. */
  help?: string;
  /** Current validation error, announced with every option. */
  error?: string;
  /** Extra classes merged onto the segmented row. */
  listClass?: string;
  /** Extra classes merged onto each option's ToggleButton. */
  optionClass?: string;
}

export function toggleGroupValue(options: readonly ToggleGroupOption[], value?: string): string {
  return options.find(option => !option.disabled && option.value === value)?.value ?? options.find(option => !option.disabled)?.value ?? "";
}

/** Roving selection skips disabled options, wraps, and respects inline direction. */
export function toggleGroupTarget(options: readonly ToggleGroupOption[], current: number, key: string, rtl = false): number | undefined {
  const enabled = options.map((option, index) => ({ option, index })).filter(({ option }) => !option.disabled);
  const position = enabled.findIndex(({ index }) => index === current);
  if (position < 0) return undefined;
  let next: number | undefined;
  if (key === "ArrowDown" || key === (rtl ? "ArrowLeft" : "ArrowRight")) next = (position + 1) % enabled.length;
  if (key === "ArrowUp" || key === (rtl ? "ArrowRight" : "ArrowLeft")) next = (position - 1 + enabled.length) % enabled.length;
  if (key === "Home") next = 0;
  if (key === "End") next = enabled.length - 1;
  return next === undefined ? undefined : enabled[next]!.index;
}
