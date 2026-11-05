import { tv } from "tailwind-variants";
import { typeRoleMerge } from "@reddb-io/design-system/base/recipes";
export const terminalSession = tv({
  slots: {
    root: "min-w-0 overflow-hidden rounded-lg border border-muted bg-background font-sans text-body text-foreground",
    header: "flex flex-wrap items-center justify-between gap-[var(--reddb-spatial-gap-md)] border-b border-muted px-[var(--reddb-spatial-inset-md)] py-[var(--reddb-spatial-inset-sm)]",
    title: "m-0 font-mono text-caption text-foreground",
    actions: "flex flex-wrap items-center gap-[var(--reddb-spatial-gap-sm)]",
    body: "relative min-w-0",
    engine: "h-full w-full overflow-hidden bg-background p-[var(--reddb-spatial-inset-md)] font-mono text-caption text-foreground focus-within:outline-2 focus-within:-outline-offset-2 focus-within:outline-focus",
    mount: "h-full w-full min-w-0 bg-background",
    fallback: "absolute inset-0 m-0 overflow-auto whitespace-pre-wrap break-words bg-background p-[var(--reddb-spatial-inset-md)] font-mono text-caption text-foreground focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-focus",
    footer: "flex flex-wrap items-center justify-between gap-[var(--reddb-spatial-gap-md)] border-t border-muted px-[var(--reddb-spatial-inset-md)] py-[var(--reddb-spatial-inset-sm)] text-caption text-ink-muted",
    transcript: "m-0 overflow-auto whitespace-pre-wrap break-words p-[var(--reddb-spatial-inset-md)] font-mono text-caption text-foreground",
  },
}, typeRoleMerge);
