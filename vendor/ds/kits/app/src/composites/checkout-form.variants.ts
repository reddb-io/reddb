import type { HTMLInputAttributes } from "svelte/elements";
import type { CheckoutField as PortableCheckoutField, CheckoutSection as PortableCheckoutSection } from "../core/checkout-form";
export { checkoutForm, type CheckoutFormVariants } from "./checkout-form.appearance";
/** Svelte native attributes on a portable checkout field. */
export interface CheckoutField extends PortableCheckoutField<HTMLInputAttributes["type"], HTMLInputAttributes["autocomplete"], HTMLInputAttributes["inputmode"]> {}
/** Svelte native attributes on a portable checkout group. */
export interface CheckoutSection extends PortableCheckoutSection<HTMLInputAttributes["type"], HTMLInputAttributes["autocomplete"], HTMLInputAttributes["inputmode"]> {}
