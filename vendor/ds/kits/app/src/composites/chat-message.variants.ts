import { tv, type VariantProps } from "tailwind-variants";

/** Density-responsive message rhythm around canonical Base media contracts. */
export const chatMessage = tv({
  slots: {
    root: "min-w-0 text-foreground",
    content: "grid min-w-0 gap-[var(--reddb-spatial-gap-md)]",
    header: "flex flex-wrap items-baseline gap-[var(--reddb-spatial-gap-sm)]",
    author: "font-medium text-foreground",
    time: "text-sm tabular-nums text-ink-muted",
    body: "min-w-0 text-sm text-foreground",
    actions: "flex flex-wrap items-center gap-[var(--reddb-spatial-gap-sm)]",
  },
});

export type ChatMessageVariants = VariantProps<typeof chatMessage>;
