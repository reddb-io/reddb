import { tv } from "tailwind-variants";
export const entityInspector = tv({
  slots: {
    root: "flex min-w-0 flex-col overflow-hidden rounded-[var(--reddb-radius-lg)] border border-muted bg-elevation-raised-surface text-foreground",
    toolbar:
      "flex min-w-0 flex-wrap items-center justify-between gap-[var(--reddb-spatial-gap-sm)] border-b border-muted p-[var(--reddb-spatial-inset-sm)]",
    navigation: "flex flex-wrap items-center gap-[var(--reddb-spatial-gap-sm)]",
    position: "text-caption tabular-nums text-ink-muted",
    body: "grid min-w-0 gap-[var(--reddb-spatial-gap-md)] p-[var(--reddb-spatial-inset-md)]",
    title: "m-0 min-w-0 [overflow-wrap:anywhere] text-heading font-medium",
    context: "m-0 min-w-0 [overflow-wrap:anywhere] text-caption text-ink-muted",
    actions: "flex flex-wrap items-center gap-[var(--reddb-spatial-gap-sm)]",
  },
});
