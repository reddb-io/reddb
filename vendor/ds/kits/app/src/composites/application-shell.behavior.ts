/** The existing shell region that owns the one application brand mark. */
export type ShellBrandRegion = "navbar" | "rail" | "panel";

/** Regions present in an application's configured shell composition. */
export interface ShellBrandComposition {
  /** Whether the shell has a Navbar brand region. */
  navbar?: boolean;
  /** Whether the shell has a SidebarRail top region. */
  rail?: boolean;
}

/**
 * Resolve the one brand owner for an application shell.
 *
 * A Navbar outranks a SidebarRail, which outranks the sidebar panel header.
 * Callers render their brand snippet only in the returned existing region.
 */
export function shellBrandRegion({
  navbar = false,
  rail = false,
}: ShellBrandComposition = {}): ShellBrandRegion {
  if (navbar) return "navbar";
  if (rail) return "rail";
  return "panel";
}
