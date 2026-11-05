/** One flat native Select option; richer groups arrive as native option children. */
export interface SelectOption {
  /** Native serialized value. */
  value: string;
  /** Visible option name. */
  label: string;
  /** Keep this option unavailable to native selection. */
  disabled?: boolean;
}

/** Native selected values; multiple selections preserve option order. */
export type SelectValue = string | number | readonly string[];

/** Read native selection for binding and reset; callbacks keep their first-value dialect. */
export function readSelectValue(element: HTMLSelectElement): string | string[] {
  return element.multiple ? Array.from(element.selectedOptions, option => option.value) : element.value;
}

/** Apply a controlled selection after native form reset without dispatching change. */
export function writeSelectValue(element: HTMLSelectElement, value: SelectValue): void {
  const values = new Set((Array.isArray(value) ? value : [value]).map(String));
  let matched = false;
  for (const option of element.options) {
    const selected: boolean = values.has(option.value) && (element.multiple || !matched);
    option.selected = selected;
    matched ||= selected;
  }
  if (!matched && !element.multiple) element.selectedIndex = -1;
}
