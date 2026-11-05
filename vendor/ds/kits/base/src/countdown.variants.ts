import { tv, type VariantProps } from "tailwind-variants";

export const countdown = tv({
  slots: {
    root: "inline-flex items-baseline gap-[var(--reddb-spatial-gap-sm)] text-foreground",
    label: "text-sm text-ink-muted",
    value: "font-mono text-2xl font-medium tabular-nums",
  },
});

export type CountdownVariants = VariantProps<typeof countdown>;
