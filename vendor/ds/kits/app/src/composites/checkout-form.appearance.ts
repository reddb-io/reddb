import { tv, type VariantProps } from "tailwind-variants";


/**
 * Token-only arrangement around canonical Base form contracts. Form and
 * Fieldset own the rhythm between their own children (ADR 0024): rows sit
 * `layout-gap-md` apart and a group's fields `layout-gap-sm`, so neither the
 * root nor a section re-spaces them.
 */
export const checkoutForm = tv({
  slots: {
    root: "w-full min-w-0",
    error: "m-0 font-medium text-feedback-danger-foreground",
    section: "min-w-0",
    actions: "flex flex-wrap items-center justify-end gap-[var(--reddb-spatial-gap-md)]",
  },
});

export type CheckoutFormVariants = VariantProps<typeof checkoutForm>;
