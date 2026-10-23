import { tv, type VariantProps } from "tailwind-variants";

const GAP = {
  sm: "gap-[var(--reddb-spatial-gap-sm)]",
  md: "gap-[var(--reddb-spatial-gap-md)]",
  lg: "gap-[var(--reddb-spatial-gap-lg)]",
} as const;

// Container-first (ADR 0021): the list is a named size container, and each
// item sets its term beside its detail from the DS's `md` container step
// (28rem) — so a list in a narrow pane stacks even on a wide window.
export const descriptionList = tv({
  slots: {
    root: "@container/description-list grid",
    item: "grid @md/description-list:grid-cols-[minmax(0,1fr)_minmax(0,2fr)]",
    term: "font-medium text-foreground",
    detail: "m-0 text-ink-muted",
  },
  variants: {
    gap: {
      sm: { root: GAP.sm, item: GAP.sm },
      md: { root: GAP.md, item: GAP.md },
      lg: { root: GAP.lg, item: GAP.lg },
    },
  },
  defaultVariants: { gap: "md" },
});

export type DescriptionListVariants = VariantProps<typeof descriptionList>;
export type DescriptionListGap = NonNullable<DescriptionListVariants["gap"]>;
export const DESCRIPTION_LIST_GAPS = Object.keys(GAP) as readonly DescriptionListGap[];

