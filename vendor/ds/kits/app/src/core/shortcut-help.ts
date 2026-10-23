export interface ShortcutCommand {
  /** Stable identity within its group. */
  id: string;
  /** Visible purpose of the command. */
  label: string;
  /** Ordered keys of one chord, supplied for the application's platform. */
  keys: readonly string[];
}
export interface ShortcutGroup {
  /** Unique group identity. */
  id: string;
  /** Context in which these commands apply. */
  label: string;
  /** Optional context explanation. */
  description?: string;
  /** Displayed commands; this component registers no keyboard handlers. */
  items: readonly ShortcutCommand[];
}
export function shortcutGroups(groups: readonly ShortcutGroup[]) {
  const ids = new Set<string>();
  for (const group of groups) {
    if (!group.id.trim() || ids.has(group.id))
      throw new Error("ShortcutHelp requires unique group ids.");
    ids.add(group.id);
    const commands = new Set<string>();
    for (const item of group.items) {
      if (
        !item.id.trim() ||
        commands.has(item.id) ||
        !item.keys.length ||
        item.keys.some((key) => !key.trim())
      )
        throw new Error(
          "ShortcutHelp requires unique command ids and non-empty chords.",
        );
      commands.add(item.id);
    }
  }
  return groups.filter((group) => group.items.length);
}
