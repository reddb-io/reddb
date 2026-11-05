import type { SectionHeadingLevel } from "@reddb-io/design-system/base";
import type { ShoppingCartItem } from "../shopping-cart";

/** Portable ShoppingCart properties shared by native adapters. */
export interface ShoppingCartContract {
/**
     * Heading text for the cart; it is also the form's accessible name unless `aria-label` is
     * given.
     */
    title: string;
/**
     * Outline depth of the cart heading, `1` to `6`; item names sit one level lower. Defaults to
     * `2`.
     */
    level?: SectionHeadingLevel;
/**
     * Caller-owned cart rows in display order, each with name, price, quantity and optional media.
     * When empty, `empty` is shown.
     */
    items: readonly ShoppingCartItem[];
/** Visible label of the total row. */
    totalLabel: string;
/** Caller-formatted total amount, announced politely whenever it changes. */
    total: string;
/** Extra classes, merged over the item slot's own, onto each cart row. */
    itemClass?: string;
/** Extra classes, merged over the quantity slot's own, onto each row's quantity input. */
    quantityClass?: string;
}
