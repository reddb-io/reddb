import type { Snippet } from "svelte";
import type { ProductListItem as PortableProductListItem } from "../core/product-list";
/** Svelte content adapter for the portable product entry. */
export interface ProductListItem extends PortableProductListItem<Snippet> {}
