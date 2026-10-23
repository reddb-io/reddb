import { tv } from "tailwind-variants";
export const operational = tv({
  slots: {
    root: "min-w-0 [overflow-wrap:anywhere] rounded-[var(--reddb-radius-lg)] border border-muted bg-elevation-raised-surface p-[var(--reddb-spatial-inset-md)] text-foreground",
    properties:
      "[overflow-wrap:anywhere] [&_[data-description-item]]:grid-cols-[minmax(0,1fr)_minmax(0,1fr)] [&_dd]:break-words [&_dd]:text-end",
    controls: "flex flex-wrap items-center gap-[var(--reddb-spatial-gap-sm)]",
    speed: "w-[calc(var(--reddb-spatial-control-height-md)*3)] min-w-0",
    seek: "grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-x-[var(--reddb-spatial-gap-md)]",
    body: "grid min-w-0 gap-[var(--reddb-spatial-gap-md)]",
    header:
      "flex min-w-0 flex-wrap items-center justify-between gap-[var(--reddb-spatial-gap-sm)]",
    title: "m-0 min-w-0 break-words text-body font-medium",
    description: "m-0 break-words text-caption text-ink-muted",
    reading: "text-body tabular-nums",
    row: "flex min-w-0 flex-wrap items-baseline justify-between gap-[var(--reddb-spatial-gap-md)]",
    list: "m-0 grid list-none gap-[var(--reddb-spatial-gap-md)] p-0",
    actions: "flex flex-wrap items-center gap-[var(--reddb-spatial-gap-sm)]",
  },
});
