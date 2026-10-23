import type { IconGlyph } from "@reddb-io/design-system/base";
  import { navigationBadge, type NavigationMarkers } from "../composites/navigation.behavior";

  export interface SidebarRailItem extends NavigationMarkers {
    /** Stable identity emitted when this item is chosen. */
    id: string;
    /**
     * Accessible name and Tooltip content for the visually compact control. A tile that shows
     * initials in icon-only mode is named `<initials>, <label>` unless `label` already holds them as words, so its
     * name starts with what it shows (WCAG 2.5.3).
     */
    label: string;
    /** Native destination; omit for an action or contextual section menu. */
    href?: string;
    /** Optional glyph rendered only through the Base Icon contract. */
    icon?: IconGlyph;
    /**
     * Initials or a short affordance such as "+" when no glyph is supplied; defaults to the first
     * two letters of `label`, upper-cased.
     */
    fallback?: string;
    /** Present but unavailable and omitted from the rail's roving focus order. */
    disabled?: boolean;
    /** Disclosure state when the selected rail item toggles an adjacent panel. */
    expanded?: boolean;
    /** Id of the adjacent panel controlled by this disclosure item. */
    controls?: string;
    /** Draw a horizontal divider before this destination to separate groups in the vertical rail. It adds no focus stop. */
    separatorBefore?: boolean;
  }

  /** The text a glyph-less tile shows: its `fallback`, else the label's first two letters. */
  export function face(item: SidebarRailItem): string {
    return item.fallback ?? item.label.slice(0, 2).toUpperCase();
  }

  const words = (text: string): string[] =>
    text.normalize("NFKD").toLowerCase().split(/[^\p{L}\p{N}]+/u).filter(Boolean);

  /**
   * A tile's accessible name. WCAG 2.5.3 (Label in Name): a speech-input user
   * says what the tile shows, so visible initials lead the name unless the
   * label already holds them as words. A glyph or a symbol such as "+" shows
   * no words, and the label alone names it.
   */
  export function nameOf(item: SidebarRailItem, labelled = false): string {
    if (item.icon) return item.label;
    if (labelled) {
      // The visible name precedes the face in reading order. Include its
      // count and initials together before the full notification description.
      return [item.label, item.badge === undefined ? undefined : navigationBadge(item.badge), face(item)].filter(Boolean).join(", ");
    }
    const shown = words(face(item));
    if (shown.length === 0) return item.label;
    const named = words(item.label);
    const held = named.some((_, start) => shown.every((word, offset) => named[start + offset] === word));
    return held ? item.label : `${face(item)}, ${item.label}`;
  }
