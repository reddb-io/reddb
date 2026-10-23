// Accordion's public item contract and token-only appearance seam.
import { tv, type VariantProps } from "tailwind-variants";

export interface AccordionItem {
  value: string;
  label: string;
  disabled?: boolean;
}

export const accordion = tv({
  slots: {
    root: "min-w-0 divide-y divide-muted border-y border-muted",
    item: "min-w-0",
    header: "m-0",
    // `group` is what the indicator's expanded-state rotation reads.
    trigger: [
      "group min-h-[var(--reddb-spatial-control-height-sm)] w-full justify-between rounded-none px-0 text-left",
      "data-[state=open]:text-foreground",
    ].join(" "),
    label: "min-w-0 truncate",
    indicator: "ml-auto shrink-0 text-ink-muted transition-transform group-aria-expanded:rotate-180 motion-reduce:transition-none",
    content: "overflow-hidden pb-[var(--reddb-spatial-inset-md)] text-foreground",
  },
});

export type AccordionVariants = VariantProps<typeof accordion>;
