import { tv } from "tailwind-variants";
import { typeRoleMerge } from "@reddb-io/design-system/base/recipes";

export const virtualList = tv({ slots: {
  root: "relative min-w-0 overflow-auto overscroll-contain rounded-md border border-muted bg-background text-foreground focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus [overflow-anchor:none]",
  probe: "pointer-events-none invisible absolute h-[var(--reddb-spatial-control-height-md)] w-px",
  list: "m-0 list-none p-0",
  row: "box-border min-w-0 overflow-hidden",
  empty: "m-0 p-[var(--reddb-spatial-inset-md)] text-body text-ink-muted",
} }, typeRoleMerge);
