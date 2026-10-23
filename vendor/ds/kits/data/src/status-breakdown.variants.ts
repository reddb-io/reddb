import { tv } from "tailwind-variants";
export const statusBreakdown = tv({
  slots: {
    root: "grid min-w-0 [overflow-wrap:anywhere] gap-[var(--reddb-spatial-gap-sm)] text-foreground",
    heading:
      "m-0 flex flex-wrap items-baseline justify-between gap-[var(--reddb-spatial-gap-md)] text-body font-medium",
    total: "text-caption font-regular tabular-nums text-ink-muted",
    track:
      "flex h-[var(--reddb-spatial-gap-md)] overflow-hidden rounded-[var(--reddb-radius-sm)] bg-muted",
    segment: "h-full min-w-0 border-e border-background last:border-e-0",
    list: "m-0 flex min-w-0 [&>li]:min-w-0 [&>li]:max-w-full list-none flex-wrap gap-x-[var(--reddb-spatial-gap-md)] gap-y-[var(--reddb-spatial-gap-sm)] p-0",
    item: "inline-flex min-w-0 max-w-full [&>span]:min-w-0 min-h-[var(--reddb-spatial-control-height-sm)] items-center gap-[var(--reddb-spatial-gap-sm)] rounded-[var(--reddb-radius-sm)] text-caption aria-pressed:font-medium aria-pressed:outline-2 aria-pressed:outline-foreground",
    swatch: "size-[var(--reddb-spatial-gap-sm)] shrink-0 rounded-full",
    count: "shrink-0 font-mono tabular-nums",
    empty: "m-0 text-caption text-ink-muted",
  },
});
