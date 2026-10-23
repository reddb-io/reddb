import { tv } from "tailwind-variants";

export const sidebarPanel = tv({
  slots: {
    root: "flex min-h-0 min-w-0 flex-col bg-elevation-sunken-surface text-foreground",
    toolbar: "flex shrink-0 items-center justify-between gap-[var(--reddb-spatial-gap-sm)] border-b border-muted px-[var(--reddb-spatial-inset-sm)] py-[var(--reddb-spatial-gap-sm)]",
    header: "flex shrink-0 min-w-0 flex-wrap items-center justify-between gap-[var(--reddb-spatial-gap-md)] px-[var(--reddb-spatial-inset-md)] py-[var(--reddb-spatial-inset-sm)]",
    actions: "ms-auto flex shrink-0 items-center gap-[var(--reddb-spatial-gap-sm)]",
    controls: "grid shrink-0 gap-[var(--reddb-spatial-gap-md)] px-[var(--reddb-spatial-inset-md)] pb-[var(--reddb-spatial-inset-sm)]",
    body: "min-h-0 min-w-0 flex-1 px-[var(--reddb-spatial-inset-sm)] pb-[var(--reddb-spatial-inset-sm)]",
    footer: "shrink-0 border-t border-muted p-[var(--reddb-spatial-inset-sm)]",
  },
});
