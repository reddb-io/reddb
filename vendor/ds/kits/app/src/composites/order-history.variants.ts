import { tv, type VariantProps } from "tailwind-variants";

/** Caller-owned order facts rendered with native table relationships. */
export interface OrderHistoryEntry {
  id: string;
  placed: string;
  placedAt: string;
  status: string;
  total: string;
}

/** Visible caller-owned column vocabulary for an order history. */
export interface OrderHistoryLabels {
  order: string;
  placed: string;
  status: string;
  total: string;
}

export const DEFAULT_ORDER_HISTORY_LABELS: OrderHistoryLabels = {
  order: "Order",
  placed: "Placed",
  status: "Status",
  total: "Total",
};

/**
 * Token-only arrangement around canonical Base heading and table contracts.
 * A group's heading sits `layout-gap-sm` above its content (ADR 0024).
 */
export const orderHistory = tv({
  slots: {
    root: "flex w-full flex-col min-w-0 gap-[var(--reddb-spatial-layout-gap-sm)]",
    heading: "pb-0",
  },
});

export type OrderHistoryVariants = VariantProps<typeof orderHistory>;
