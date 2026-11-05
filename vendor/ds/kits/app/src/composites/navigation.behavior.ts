import type { IconGlyph, Tone } from "@reddb-io/design-system/base";

/** Application-owned presence or health, independent of navigation selection. */
export interface NavigationStatus {
  /** Meaning announced alongside the destination, such as Online or Service unavailable. */
  label: string;
  /** Semantic tone of the status mark. Defaults to neutral. */
  tone?: Tone;
}

/** Optional notifications and status shared by the sidebar surfaces. */
export interface NavigationMarkers {
  /** Notification count or short text; undefined omits it. Numeric counts above 99 display 99+ while retaining the exact accessible count. */
  badge?: number | string;
  /** Complete accessible notification description, e.g. 91 unread conversations. */
  badgeLabel?: string;
  /** Semantic notification tone. Defaults to neutral. */
  badgeTone?: Tone;
  /** Presence or health indicator; its required label carries meaning beyond colour. */
  status?: NavigationStatus;
}

export function navigationBadge(value: number | string): string {
  return typeof value === "number" && value > 99 ? "99+" : String(value);
}

export function navigationMarkerLabel(item: NavigationMarkers, label: string): string {
  const visible = item.badge === undefined ? undefined : navigationBadge(item.badge);
  const description = item.badgeLabel ?? `${item.badge} notifications`;
  const notification = visible === undefined ? undefined : description.includes(visible) ? description : `${visible} (${description})`;
  return [label, notification, item.status?.label].filter(Boolean).join(", ");
}

/** One destination shared by the Application Kit's navigation surfaces. */
export interface ApplicationNavigationItem extends NavigationMarkers {
  /** Stable identity and the value matched by a surface's required `currentId`. */
  id: string;
  /** Visible destination name. */
  label: string;
  /** Native destination, kept mandatory so navigation remains keyboard reachable. */
  href: string;
  /** Optional decorative glyph, drawn through the Base Icon contract beside the label. */
  icon?: IconGlyph;
  /** Present but unavailable and removed from the tab order by canonical NavItem. */
  disabled?: boolean;
  /** Optional consumer action performed when the destination is chosen. */
  onselect?: () => void;
}

/** A destination after the surface has resolved its single current item. */
export interface ResolvedApplicationNavigationItem extends ApplicationNavigationItem {
  current: boolean;
}

/** Resolve current-item state once, refusing ambiguous or missing identities. */
export function navigationItems<T extends { id: string }>(
  items: readonly T[],
  currentId: string,
): readonly (T & { current: boolean })[] {
  const matches = items.filter((item) => item.id === currentId);
  if (matches.length !== 1) {
    throw new Error(`currentId "${currentId}" must identify exactly one navigation item`);
  }

  return items.map((item) => ({ ...item, current: item.id === currentId }));
}

/** A primary destination. Without href, activation opens its contextual menu. */
export interface PrimaryNavigationItem extends NavigationMarkers {
  /** Stable section identity; distinct from whether its menu is open. */
  id: string;
  /** Visible label on the mobile bar and accessible desktop name. */
  label: string;
  /** Native link destination. Omit for a section with a contextual sidebar. */
  href?: string;
  /** Decorative glyph rendered through Base Icon. */
  icon?: IconGlyph;
  /** Optional initials when there is no glyph. */
  fallback?: string;
  /** Unavailable entries retain their place and cannot be activated. */
  disabled?: boolean;
  /** Optional action when activated; routing and currentId remain application-owned. */
  onselect?: () => void;
}
