import { tv, type VariantProps } from "tailwind-variants";

/** Token-only supplemental content appearance; behavior remains in Tooltip.svelte. */
export const tooltip = tv({
  base: [
    "z-50 whitespace-nowrap rounded-md border border-elevation-overlay-border bg-elevation-overlay-surface text-foreground shadow-elevation-overlay",
    // One line of xs copy on a control-height-sm band: it measured 40px at the
    // neutral stop with inset padding above and below.
    "flex min-h-[var(--reddb-spatial-control-height-sm)] items-center px-[var(--reddb-spatial-inset-sm)] text-xs",
    "motion-safe:transition-opacity motion-reduce:transition-none",
  ].join(" "),
});

export type TooltipVariants = VariantProps<typeof tooltip>;
