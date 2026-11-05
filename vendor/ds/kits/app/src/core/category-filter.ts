/** Preserve selection order and avoid duplicates while adding or removing one category. */
export function categoryValues(active: readonly string[], value: string, checked: boolean): string[] {
  return checked ? active.includes(value) ? [...active] : [...active, value] : active.filter(candidate => candidate !== value);
}
