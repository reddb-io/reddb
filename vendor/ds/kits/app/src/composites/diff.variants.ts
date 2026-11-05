import { tv, type VariantProps } from "tailwind-variants";

export const DIFF_MODES = ["inline", "side-by-side"] as const;
export type DiffMode = (typeof DIFF_MODES)[number];
export type DiffChangeKind = "context" | "addition" | "removal" | "empty";

const CHANGE: Record<DiffChangeKind, string> = {
  context: "bg-transparent text-foreground",
  addition:
    "bg-[var(--reddb-color-feedback-success-surface)] text-[var(--reddb-color-feedback-success-foreground)]",
  removal:
    "bg-[var(--reddb-color-feedback-danger-surface)] text-[var(--reddb-color-feedback-danger-foreground)]",
  empty: "invisible",
};

export const diff = tv({
  slots: {
    root: "w-full min-w-0 overflow-hidden rounded-lg border border-muted bg-transparent text-foreground",
    split: "w-full min-w-0",
    pane: "min-w-0",
    heading:
      "border-b border-muted px-[var(--reddb-spatial-inset-md)] py-[var(--reddb-spatial-inset-sm)] text-sm font-medium text-foreground",
    lines: "m-0 overflow-x-auto py-[var(--reddb-spatial-inset-sm)] font-mono text-sm leading-relaxed",
    line: "grid grid-cols-[auto_1fr] gap-[var(--reddb-spatial-gap-sm)] px-[var(--reddb-spatial-inset-md)] whitespace-pre",
    marker: "select-none",
  },
  variants: { change: CHANGE },
  defaultVariants: { change: "context" },
});

export type DiffVariants = VariantProps<typeof diff>;
