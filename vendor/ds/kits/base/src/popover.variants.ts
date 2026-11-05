import { tv, type VariantProps } from "tailwind-variants";

/** Token-only anchored surface appearance; positioning and focus remain in Popover.svelte. */
export const popover = tv({
  base: [
    "z-50 w-80 max-w-sm rounded-lg border border-elevation-overlay-border bg-elevation-overlay-surface text-elevation-overlay-foreground shadow-elevation-overlay",
    "p-[var(--reddb-spatial-inset-md)] outline-hidden",
    "motion-safe:transition-opacity motion-reduce:transition-none",
  ].join(" "),
});

export type PopoverVariants = VariantProps<typeof popover>;
