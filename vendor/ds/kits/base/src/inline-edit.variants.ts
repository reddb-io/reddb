import { tv } from "tailwind-variants";
import { typeRoleMerge } from "./type-roles";

export const inlineEdit = tv({
  slots: {
    root: "group/inline-edit flex min-w-0 flex-col gap-[var(--reddb-spatial-gap-sm)] text-foreground",
    label: "text-caption text-ink-muted",
    row: "flex min-w-0 flex-wrap items-center gap-[var(--reddb-spatial-gap-sm)]",
    value: "min-w-0 break-words",
    field: "relative min-w-0 flex-1 basis-[12ch]",
    input: "min-w-0 w-full",
    icon: "pointer-events-none inline-flex shrink-0 items-center text-ink-muted",
    inputIcon: "pointer-events-none absolute inset-y-0 start-0 inline-flex w-[var(--reddb-spatial-control-height-md)] items-center justify-center text-ink-muted",
    readText: "min-w-0 select-text break-words [overflow-wrap:anywhere]",
    status: "m-0 break-words text-caption text-ink-muted",
    edit: "shrink-0",
    error: "m-0 text-caption text-feedback-danger-foreground",
  },
  variants: {
    presentation: {
      text: { value: "inline-flex items-center gap-[var(--reddb-spatial-gap-sm)] text-body font-medium" },
      input: { value: "flex min-h-[var(--reddb-spatial-control-height-md)] flex-1 basis-[12ch] items-center gap-[var(--reddb-spatial-gap-sm)] text-editable-md" },
    },
    withIcon: {
      true: { input: "ps-[var(--reddb-spatial-control-height-md)]" },
    },
    editVisibility: {
      always: { edit: "" },
      hover: { edit: "[@media(hover:hover)_and_(pointer:fine)]:opacity-0 group-hover/inline-edit:opacity-100 group-focus-within/inline-edit:opacity-100" },
    },
  },
  defaultVariants: { editVisibility: "always", presentation: "text" },
}, typeRoleMerge);
