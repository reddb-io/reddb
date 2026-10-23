// Switch's optional appearance seam. The native checkbox supplies binary and
// form semantics; the moving thumb makes that state visible without color.

import { tv, type VariantProps } from "tailwind-variants";

export const switchControl = tv({
  base: [
    "relative h-[var(--reddb-spatial-control-height-sm)] w-[max(var(--reddb-spatial-target-min),calc(var(--reddb-spatial-switch-track-height)*1.75))] shrink-0 appearance-none",
    "cursor-pointer rounded-full border-0 bg-transparent",
    "after:pointer-events-none after:absolute after:start-1/2 after:top-1/2 after:-translate-x-1/2 after:-translate-y-1/2 rtl:after:translate-x-1/2",
    "after:h-[var(--reddb-spatial-switch-track-height)] after:w-[calc(var(--reddb-spatial-switch-track-height)*1.75)] after:rounded-full after:border after:border-control-edge after:bg-muted checked:after:bg-primary enabled:hover:after:border-foreground enabled:checked:hover:after:bg-primary-active",
    "before:pointer-events-none before:absolute before:z-10 before:top-1/2 before:-translate-y-1/2 before:start-[calc(50%-var(--reddb-spatial-switch-track-height)*0.875+(var(--reddb-spatial-switch-track-height)-var(--reddb-spatial-switch-thumb-size))*0.5)]",
    "before:h-[var(--reddb-spatial-switch-thumb-size)] before:aspect-square before:rounded-full before:bg-ink-muted checked:before:bg-on-primary before:transition-transform checked:before:translate-x-[calc(var(--reddb-spatial-switch-track-height)*0.75)] rtl:checked:before:-translate-x-[calc(var(--reddb-spatial-switch-track-height)*0.75)]",
    "motion-reduce:before:transition-none",
    "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus",
    "aria-invalid:after:border-feedback-danger-border disabled:cursor-not-allowed disabled:opacity-50",
  ].join(" "),
});

export type SwitchVariants = VariantProps<typeof switchControl>;
