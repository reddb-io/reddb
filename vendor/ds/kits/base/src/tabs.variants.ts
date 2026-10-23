// Tabs' token-only appearance seam; Bits UI owns selection and roving focus.
import { tv, type VariantProps } from "tailwind-variants";

export interface TabItem {
  value: string;
  label: string;
  disabled?: boolean;
}

export const tabs = tv({
  slots: {
    root: "min-w-0",
    list: "flex flex-wrap gap-[var(--reddb-spatial-gap-sm)] border-b border-muted",
    // The active tab wears the shared selection language (DESIGN.md,
    // "Interactive states"): a neutral surface, full-weight ink, and a 2px
    // ink underline on the edge it shares with the list's rule. Every
    // trigger carries the same 2px edge — transparent until active — so
    // selecting a tab moves nothing.
    trigger: [
      "-mb-px h-[var(--reddb-spatial-control-height-sm)] rounded-b-none border-0 border-b-2 border-transparent",
      "data-[state=active]:border-foreground data-[state=active]:bg-foreground/10",
      "data-[state=active]:font-medium data-[state=active]:text-foreground",
      "data-[state=inactive]:text-ink-muted",
    ].join(" "),
    content: [
      "py-[var(--reddb-spatial-inset-md)] text-foreground",
      "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus",
    ].join(" "),
  },
});

export type TabsVariants = VariantProps<typeof tabs>;
