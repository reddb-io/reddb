import { tv } from "tailwind-variants";
export const phoneInput = tv({
  slots: {
    root: "min-w-0",
    row: "grid min-w-0 grid-cols-[minmax(0,14ch)_minmax(0,1fr)] items-start gap-[var(--reddb-spatial-gap-sm)]",
    country: "min-w-0 [&>label]:sr-only",
    input: "min-w-0 w-full tabular-nums",
    popup: "min-w-[min(32ch,calc(100vw-var(--reddb-spatial-inset-lg)*2))]",
    read: "grid min-w-0 gap-[var(--reddb-spatial-gap-sm)]",
    label: "text-sm font-medium text-foreground",
    value: "flex min-w-0 min-h-[var(--reddb-spatial-control-height-md)] items-center gap-[var(--reddb-spatial-gap-sm)] text-editable-md",
    number: "min-w-0 select-text break-words [overflow-wrap:anywhere]",
    flag: "shrink-0",
    help: "m-0 text-caption text-ink-muted",
    error: "m-0 text-caption text-feedback-danger-foreground",
  },
});
