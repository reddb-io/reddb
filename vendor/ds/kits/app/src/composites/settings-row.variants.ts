import { tv } from "tailwind-variants";

/** One Density-responsive composition for field associations and independently named actions. */
export const settingsRow = tv({
  slots: {
    root: "@container/settings-row min-w-0 border-b border-muted py-[var(--reddb-spatial-inset-md)] last:border-b-0",
    row: "grid min-w-0 gap-[var(--reddb-spatial-layout-gap-sm)] @sm/settings-row:grid-cols-[minmax(0,1fr)_minmax(0,1fr)] @sm/settings-row:items-center",
    copy: "grid min-w-0 content-start gap-[var(--reddb-spatial-gap-sm)]",
    label: "m-0 break-words text-body font-medium text-foreground",
    description: "m-0 break-words text-caption text-ink-muted",
    control: "min-w-0",
    end: "grid min-w-0 content-start gap-[var(--reddb-spatial-gap-sm)]",
    actions: "flex min-w-0 flex-wrap items-center gap-[var(--reddb-spatial-gap-sm)] @sm/settings-row:justify-end",
    required: "ms-[var(--reddb-spatial-gap-sm)]",
    error: "m-0 break-words text-caption text-feedback-danger-foreground",
  },
});
