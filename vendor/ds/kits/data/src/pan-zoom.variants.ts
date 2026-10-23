import { tv } from "tailwind-variants";
import { typeRoleMerge } from "@reddb-io/design-system/base/recipes";

export const panZoom = tv({ slots: {
  root: "flex min-w-0 flex-col gap-[var(--reddb-spatial-gap-sm)] text-foreground",
  viewport: "relative min-w-0 overflow-hidden rounded-md border border-muted bg-elevation-sunken-surface [touch-action:none] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus",
  content: "absolute left-0 top-0 origin-top-left",
  controls: "flex flex-wrap items-center gap-[var(--reddb-spatial-gap-sm)]",
  status: "min-w-[5ch] text-center font-mono text-caption text-ink-muted",
} }, typeRoleMerge);
