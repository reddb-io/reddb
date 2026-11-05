import { tv, type VariantProps } from "tailwind-variants";

/** Density-responsive review rhythm around canonical Base contracts. */
export const review = tv({
  slots: {
    root: "w-full min-w-0 text-foreground",
    card: "w-full",
    identity: "grid min-w-0 gap-[var(--reddb-spatial-gap-sm)]",
    author: "font-medium text-foreground",
    time: "text-sm tabular-nums text-ink-muted",
    content: "grid w-full min-w-0 gap-[var(--reddb-spatial-gap-md)]",
    body: "min-w-0 text-sm text-foreground",
    actions: "flex flex-wrap items-center gap-[var(--reddb-spatial-gap-sm)]",
  },
});

export type ReviewVariants = VariantProps<typeof review>;
