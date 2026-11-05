export interface InspectorEntity {
  /** Stable application-owned identity. */
  id: string;
  /** Visible entity title. */
  label: string;
  /** Supporting context beneath the title. */
  description?: string;
}
export function inspectedEntity(
  items: readonly InspectorEntity[],
  selected: string | null,
) {
  const ids = new Set<string>();
  for (const item of items) {
    if (!item.id || ids.has(item.id))
      throw new Error("EntityInspector: ids must be non-empty and unique.");
    ids.add(item.id);
  }
  const index = items.findIndex((item) => item.id === selected);
  return {
    entity: items[index],
    index,
    previous: items[index - 1]?.id,
    next: index < 0 ? undefined : items[index + 1]?.id,
  };
}
