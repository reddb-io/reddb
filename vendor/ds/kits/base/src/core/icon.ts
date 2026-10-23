/** Density-owned icon sizes shared by native adapters. */
export const ICON_SIZES = ["sm", "md", "lg"] as const;
export type IconSize = (typeof ICON_SIZES)[number];
/** Semantic ink roles accepted by Icon. */
export const ICON_COLORS = ["current", "foreground", "ink-muted", "primary", "on-primary", "feedback-danger-foreground", "feedback-success-foreground", "feedback-warning-foreground", "feedback-info-foreground"] as const;
export type IconColor = (typeof ICON_COLORS)[number];
/** @deprecated Use ink-muted; muted is a surface role. */
export type DeprecatedIconColor = "muted";
/** Resolve a semantic role, including the retained deprecated alias. */
export function iconColor(color: IconColor | DeprecatedIconColor): string {
  const role = color === "muted" ? "ink-muted" : color;
  return role === "current" ? "currentColor" : `var(--reddb-color-${role})`;
}
/** Density controls glyph dimensions independently of the control hit area. */
export function iconDimension(size: IconSize): string { return `var(--reddb-spatial-icon-size-${size})`; }
