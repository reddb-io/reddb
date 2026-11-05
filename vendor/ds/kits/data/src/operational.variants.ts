import { tv } from "tailwind-variants";
const cellItem = "row-span-3 grid min-w-0 grid-rows-subgrid";

export const operationalView = tv({
  slots: {
    scene: "relative min-w-0",
    sceneFigure: "m-0 min-w-0",
    sceneContent: "relative h-full w-full",
    staticDrawing: "relative",
    overlay: "pointer-events-none absolute inset-0 h-full w-full",
    engine: "pointer-events-none absolute inset-0",
    caption: "mb-[var(--reddb-spatial-gap-md)] text-body font-medium",
    probe:
      "pointer-events-none invisible absolute h-[var(--reddb-spatial-control-height-md)]",
    nodeTitle: "text-body font-medium",
    nodeText: "text-caption",
    srOnly: "sr-only",
    count: "tabular-nums",
    queue:
      "m-0 grid list-none grid-cols-[repeat(auto-fit,minmax(min(100%,12ch),1fr))] gap-[var(--reddb-spatial-gap-md)] p-0",
    queueSlot: cellItem,
    stateItem: cellItem,
    eventBody:
      "grid min-w-0 w-full gap-[var(--reddb-spatial-gap-sm)] text-caption",
    eventMeta: "flex min-w-0 [&>time]:shrink-0 [&>span]:min-w-0 [&>span]:truncate gap-[var(--reddb-spatial-gap-md)] text-ink-muted",
    eventMessage: "m-0 truncate text-foreground",
    note: "text-caption text-ink-muted",
    entityList:
      "m-0 flex list-none flex-wrap gap-[var(--reddb-spatial-gap-md)] p-0 text-caption text-ink-muted",
    root: "min-w-0 [overflow-wrap:anywhere] text-foreground",
    header:
      "mb-[var(--reddb-spatial-gap-md)] flex flex-wrap items-baseline justify-between gap-[var(--reddb-spatial-gap-md)]",
    title: "m-0 break-words text-body font-medium",
    text: "m-0 break-words text-caption text-ink-muted",
    grid: "m-0 grid list-none gap-[var(--reddb-spatial-gap-sm)] p-0",
    cell: "row-span-3 grid h-full min-w-0 grid-rows-subgrid gap-[var(--reddb-spatial-gap-sm)] rounded-[var(--reddb-radius-md)] border border-control-edge bg-elevation-raised-surface p-[var(--reddb-spatial-inset-sm)] text-start text-foreground",
    selected:
      "bg-foreground/10 font-medium outline-2 outline-offset-2 outline-foreground",
    event:
      "flex min-w-0 flex-wrap items-baseline gap-[var(--reddb-spatial-gap-md)] border-b border-muted py-[var(--reddb-spatial-inset-sm)] text-caption",
    figure: "m-0 grid min-w-0 gap-[var(--reddb-spatial-gap-md)]",
    svg: "block h-auto w-full overflow-visible text-foreground",
    metrics:
      "grid min-w-0 grid-cols-[repeat(auto-fit,minmax(min(100%,14rem),1fr))] gap-[var(--reddb-spatial-layout-gap-md)] [&>[data-kpi-card]]:row-span-2 [&>[data-kpi-card]]:grid-rows-subgrid",
  },
});
