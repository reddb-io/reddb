import { tv } from "tailwind-variants";
import { typeRoleMerge } from "@reddb-io/design-system/base/recipes";
export const entityJourney = tv(
  {
    slots: {
      root: "@container/entity-journey min-w-0 text-foreground",
      list: "m-0 grid min-w-0 list-none grid-cols-1 gap-[var(--reddb-spatial-gap-sm)] p-0 @lg/entity-journey:grid-cols-[repeat(auto-fit,minmax(min(100%,calc(var(--reddb-spatial-control-height-md)*4)),1fr))]",
      stage:
        "min-w-0 @lg/entity-journey:row-span-4 @lg/entity-journey:grid @lg/entity-journey:grid-rows-subgrid",
      item: "flex h-full w-full min-w-0 items-start @lg/entity-journey:row-span-4 @lg/entity-journey:grid @lg/entity-journey:grid-cols-[auto_minmax(0,1fr)] @lg/entity-journey:grid-rows-subgrid gap-[var(--reddb-spatial-gap-sm)] rounded-[var(--reddb-radius-md)] border border-transparent p-[var(--reddb-spatial-inset-sm)] text-start aria-pressed:border-foreground aria-pressed:bg-foreground/10",
      marker:
        "flex size-[var(--reddb-spatial-control-height-sm)] shrink-0 items-center justify-center rounded-full border border-control-edge text-caption tabular-nums",
      body: "grid min-w-0 flex-1 @lg/entity-journey:col-start-2 @lg/entity-journey:row-span-4 @lg/entity-journey:grid-rows-subgrid gap-[var(--reddb-spatial-gap-sm)] [overflow-wrap:anywhere]",
      label: "text-body font-medium",
      note: "text-caption text-ink-muted empty:hidden @lg/entity-journey:empty:block",
      status: "text-caption text-ink-muted",
      elapsed:
        "text-caption tabular-nums text-ink-muted empty:hidden @lg/entity-journey:empty:block",
    },
    variants: {
      state: {
        complete: { marker: "bg-foreground text-background border-foreground" },
        current: { marker: "border-2 border-foreground font-bold" },
        upcoming: { marker: "text-ink-muted" },
        blocked: {
          marker:
            "text-feedback-warning-foreground border-feedback-warning-foreground",
        },
        failed: {
          marker:
            "text-feedback-danger-foreground border-feedback-danger-foreground",
        },
        skipped: { marker: "text-ink-muted border-dashed" },
      },
    },
  },
  typeRoleMerge,
);
