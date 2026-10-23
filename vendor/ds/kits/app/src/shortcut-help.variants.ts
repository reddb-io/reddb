import { tv } from "tailwind-variants";
import { typeRoleMerge } from "@reddb-io/design-system/base/recipes";
export const shortcutHelp = tv(
  {
    slots: {
      root: "@container/shortcut-help grid min-w-0 gap-[var(--reddb-spatial-layout-gap-md)] text-foreground",
      group: "grid min-w-0 gap-[var(--reddb-spatial-gap-md)]",
      title: "m-0 text-heading [overflow-wrap:anywhere]",
      description: "m-0 text-caption text-ink-muted [overflow-wrap:anywhere]",
      list: "m-0 grid min-w-0 list-none p-0",
      item: "flex min-w-0 flex-wrap items-center justify-between gap-[var(--reddb-spatial-gap-sm)] border-b border-muted py-[var(--reddb-spatial-inset-sm)]",
      name: "min-w-0 text-body [overflow-wrap:anywhere]",
      keys: "inline-flex max-w-full flex-wrap items-center gap-[var(--reddb-spatial-gap-sm)]",
    },
  },
  typeRoleMerge,
);
