import { tv, type VariantProps } from "tailwind-variants";
import { typeRoleMerge } from "./type-roles";

/**
 * Native-link appearance with a persistent non-colour affordance.
 *
 * A link is ink, not accent: foreground text carrying the underline, which
 * thickens on hover. The Brand red is reserved for the primary action — and in
 * the light scheme it is the same red as danger, so a red link would read as an
 * error (DESIGN.md, "Interactive states").
 */
export const link = tv({
  base: [
    "text-foreground underline decoration-current underline-offset-[var(--reddb-spatial-gap-sm)]",
    "hover:decoration-2",
    "focus-visible:rounded-sm focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus",
  ].join(" "),
}, typeRoleMerge);

export type LinkVariants = VariantProps<typeof link>;
