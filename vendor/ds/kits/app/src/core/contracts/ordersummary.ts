import type { SectionHeadingLevel } from "@reddb-io/design-system/base";
import type { OrderSummaryLine } from "../../composites/order-summary.variants";

/** Portable OrderSummary properties shared by native adapters. */
export interface OrderSummaryContract {
/**
     * Heading text for the summary; it is also the region's accessible name unless `aria-label` is
     * given.
     */
    title: string;
/** Outline depth of the heading, `1` to `6`. Defaults to `2`. */
    level?: SectionHeadingLevel;
/** Caller-owned amounts or facts shown as label and value pairs above the total, in order. */
    lines: readonly OrderSummaryLine[];
/** Visible label of the total row. */
    totalLabel: string;
/** Caller-formatted total amount, announced politely whenever it changes. */
    total: string;
}
