import { tv } from "tailwind-variants";
import { typeRoleMerge } from "@reddb-io/design-system/base/recipes";

export const geoMap = tv({ slots: {
  root: "relative m-0 flex min-w-0 flex-col gap-[var(--reddb-spatial-gap-md)] text-foreground",
  title: "m-0 text-body font-medium",
  description: "m-0 max-w-prose text-caption text-ink-muted",
  svg: "block h-full w-full text-foreground",
  data: "text-caption text-ink-muted",
  list: "m-0 mt-[var(--reddb-spatial-gap-sm)] grid list-none gap-[var(--reddb-spatial-gap-sm)] p-0",
  row: "flex flex-wrap items-center justify-between gap-[var(--reddb-spatial-gap-sm)] border-b border-muted pb-[var(--reddb-spatial-inset-sm)]",
} }, typeRoleMerge);
