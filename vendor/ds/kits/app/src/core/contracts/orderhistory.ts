import type { SectionHeadingLevel } from "@reddb-io/design-system/base";
import type { OrderHistoryEntry, OrderHistoryLabels } from "../../composites/order-history.variants";

/** Portable OrderHistory properties shared by native adapters. */
export interface OrderHistoryContract {
/**
     * Heading text for the history; it is also the section's accessible name unless `aria-label` is
     * given.
     */
    title: string;
/** Outline depth of the heading, `1` to `6`. Defaults to `2`. */
    level?: SectionHeadingLevel;
/** Caption of the order table, shown above it and used as the table's accessible name. */
    caption: string;
/**
     * Caller-owned orders in display order, one row each: `id`, `placed`, `placedAt`, `status` and
     * `total`.
     */
    orders: readonly OrderHistoryEntry[];
/**
     * Overrides for the column headers (`order`, `placed`, `status`, `total`); omitted keys keep
     * their defaults.
     */
    labels?: Partial<OrderHistoryLabels>;
}
