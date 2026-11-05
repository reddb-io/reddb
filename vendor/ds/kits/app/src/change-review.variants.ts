import { tv } from "tailwind-variants";
import { typeRoleMerge } from "@reddb-io/design-system/base/recipes";
export const changeReview = tv(
  {
    slots: {
      root: "@container/change-review flex min-w-0 flex-col gap-[var(--reddb-spatial-layout-gap-sm)] text-foreground",
      title: "m-0 text-heading",
      list: "m-0 grid min-w-0 list-none divide-y divide-muted p-0",
      item: "grid min-w-0 gap-[var(--reddb-spatial-gap-md)] py-[var(--reddb-spatial-inset-md)]",
      header:
        "flex min-w-0 flex-wrap items-start justify-between gap-[var(--reddb-spatial-gap-sm)]",
      name: "min-w-0 flex-1 text-body font-medium [overflow-wrap:anywhere]",
      context: "m-0 text-caption text-ink-muted [overflow-wrap:anywhere]",
      comparison:
        "m-0 grid min-w-0 gap-[var(--reddb-spatial-gap-sm)] @md/change-review:grid-cols-2",
      pair: "grid min-w-0 content-start gap-[var(--reddb-spatial-gap-sm)]",
      term: "text-caption text-ink-muted",
      value:
        "m-0 whitespace-pre-wrap rounded-[var(--reddb-radius-sm)] bg-elevation-sunken-surface p-[var(--reddb-spatial-inset-sm)] font-mono text-body [overflow-wrap:anywhere]",
      error:
        "m-0 text-body text-feedback-danger-foreground [overflow-wrap:anywhere]",
      actions:
        "flex flex-wrap items-center justify-end gap-[var(--reddb-spatial-gap-sm)] border-t border-muted pt-[var(--reddb-spatial-inset-md)]",
    },
  },
  typeRoleMerge,
);
