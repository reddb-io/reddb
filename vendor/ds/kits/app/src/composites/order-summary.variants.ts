import { tv, type VariantProps } from "tailwind-variants";

/** One caller-owned amount or fact in an order summary. */
export interface OrderSummaryLine {
  label: string;
  value: string;
}

/**
 * Token-only arrangement around canonical Base Card and DescriptionList. The
 * lines and the total are two blocks, `layout-gap-sm` apart (ADR 0024).
 */
export const orderSummary = tv({
  slots: {
    root: "w-full min-w-0",
    heading: "pb-0",
    content: "flex flex-col gap-[var(--reddb-spatial-layout-gap-sm)]",
    total: "flex items-center justify-between gap-[var(--reddb-spatial-gap-md)] font-medium",
    actions: "flex flex-wrap items-center justify-end gap-[var(--reddb-spatial-gap-md)]",
  },
});

export type OrderSummaryVariants = VariantProps<typeof orderSummary>;
