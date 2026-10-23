import type { CheckoutSection } from "../checkout-form";

/** Portable CheckoutForm properties shared by native adapters. */
export interface CheckoutFormContract {
/** Caller-owned field groups; the component owns only their associations. */
    sections?: readonly CheckoutSection[];
/** Form-level failure associated with the native form boundary. */
    error?: string;
/** Default native submit label when no actions snippet is supplied. */
    submitLabel?: string;
/** Extra classes, merged over the section slot's own, onto each section's fieldset. */
    sectionClass?: string;
/**
     * Extra classes, merged over the actions slot's own, onto the wrapper around the actions or
     * submit button.
     */
    actionsClass?: string;
}
