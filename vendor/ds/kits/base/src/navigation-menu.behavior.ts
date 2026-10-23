/** A native destination rendered by NavigationMenu. */
export interface NavigationMenuLink {
  id: string;
  label: string;
  href: string;
  description?: string;
  active?: boolean;
  onselect?: () => void;
}

/** A top-level control whose links open in an anchored surface. */
export interface NavigationMenuSection {
  id: string;
  label: string;
  links: readonly NavigationMenuLink[];
}

export type NavigationMenuEntry = NavigationMenuLink | NavigationMenuSection;

export function isNavigationMenuSection(
  entry: NavigationMenuEntry,
): entry is NavigationMenuSection {
  return Array.isArray((entry as NavigationMenuSection).links);
}
