
/** One selectable row in a DropdownMenu. */
export interface DropdownMenuItem<Glyph = unknown> {
  id: string;
  label: string;
  /** Optional decorative glyph rendered before the label through the DS Icon contract. */
  icon?: Glyph;
  /** A keyboard shortcut rendered as semantic key caps after the label. */
  shortcut?: readonly string[];
  href?: string;
  disabled?: boolean;
  onselect?: () => void;
}

/** A named or unnamed run of related menu rows. */
export interface DropdownMenuGroup<Glyph = unknown> {
  heading?: string;
  items: readonly DropdownMenuItem<Glyph>[];
}

export type DropdownMenuEntry<Glyph = unknown> = DropdownMenuItem<Glyph> | DropdownMenuGroup<Glyph>;

export function isDropdownMenuGroup<Glyph>(entry: DropdownMenuEntry<Glyph>): entry is DropdownMenuGroup<Glyph> {
  return Array.isArray((entry as DropdownMenuGroup<Glyph>).items);
}

/** Fold consecutive bare rows into groups while retaining explicit group boundaries. */
export function dropdownMenuGroups<Glyph>(
  entries: readonly DropdownMenuEntry<Glyph>[],
): readonly DropdownMenuGroup<Glyph>[] {
  const groups: DropdownMenuGroup<Glyph>[] = [];
  let run: DropdownMenuItem<Glyph>[] | undefined;

  for (const entry of entries) {
    if (isDropdownMenuGroup(entry)) {
      run = undefined;
      if (entry.items.length > 0) groups.push(entry);
      continue;
    }
    if (run === undefined) {
      run = [];
      groups.push({ items: run });
    }
    run.push(entry);
  }

  return groups;
}

/** Flatten grouped entries back into their source-order menu rows. */
export function dropdownMenuItems<Glyph>(
  entries: readonly DropdownMenuEntry<Glyph>[],
): readonly DropdownMenuItem<Glyph>[] {
  return dropdownMenuGroups(entries).flatMap((group) => [...group.items]);
}
