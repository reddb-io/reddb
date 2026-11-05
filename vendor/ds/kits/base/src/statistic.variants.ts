import { tv, type VariantProps } from "tailwind-variants";

export const statistic = tv({
  slots: {
    root: "min-w-0",
    detail: "grid gap-[var(--reddb-spatial-gap-sm)]",
    value: "font-mono text-2xl font-medium tabular-nums text-foreground",
    description: "text-sm text-ink-muted",
  },
});

export type StatisticVariants = VariantProps<typeof statistic>;
