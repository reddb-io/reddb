// Disclosure's token-only appearance seam; Collapsible owns expanded state.
import { tv, type VariantProps } from "tailwind-variants";

export const disclosure = tv({
  slots: {
    root: "min-w-0 rounded-md border border-muted",
    // `group` is what the indicator's expanded-state rotation reads.
    trigger: "group w-full justify-between",
    indicator: "ml-auto text-ink-muted transition-transform group-aria-expanded:rotate-180 motion-reduce:transition-none",
    content: "border-t border-muted p-[var(--reddb-spatial-inset-md)] text-foreground",
  },
});

export type DisclosureVariants = VariantProps<typeof disclosure>;
