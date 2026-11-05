import type { AccordionItem } from "../../accordion.variants";

/** Canonical portable Accordion properties, shared by native adapters. */
export interface AccordionContract {
/** Accessible name for the collection of sections. */
    label: string;
/**
     * The sections, in order: each has a unique `value`, a visible `label` and an optional
     * `disabled` flag.
     */
    items: readonly AccordionItem[];
/** Expanded item values, bindable (ADR 0026). Single mode retains at most one value. */
    value?: string[];
/**
     * Lets several sections stay open at once. When omitted, opening a section closes the previous
     * one.
     */
    multiple?: boolean;
/**
     * Disables the whole Accordion so no section can be toggled; one section is disabled through
     * its own item.
     */
    disabled?: boolean;
/** Extra classes merged onto every section's trigger Button. */
    triggerClass?: string;
/** Extra classes merged onto every section's content region. */
    contentClass?: string;
}
