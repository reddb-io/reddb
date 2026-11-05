import type { Snippet } from "svelte";
import type { ShoppingCartItem as PortableShoppingCartItem } from "../core/shopping-cart";
export { shoppingCart, type ShoppingCartVariants } from "./shopping-cart.appearance";
/** Svelte content adapter for a portable cart row. */
export interface ShoppingCartItem extends PortableShoppingCartItem<Snippet> {}
