import type { SearchSelectOption } from "../../search-select.variants";
import type { InputSize } from "../../input.variants";

/** Shared canonical SearchSelect properties. */
export interface SearchSelectContract {
/** Visible field label, also announced with the invoking control. */
    label: string;
/** Caller-owned choices with optional descriptions, search aliases and disabled states. */
    options: readonly SearchSelectOption[];
/** Native form name; multiple selection submits one entry per selected value. */
    name?: string;
/** Supporting text associated with the invoking control. */
    help?: string;
/** Validation error associated with the invoking control. */
    error?: string;
/** Enforces at least one selected value through native form validation. */
    required?: boolean;
/** Disables selection, clearing and chip removal, and excludes the value from form submission. */
    disabled?: boolean;
/** Text shown when the selection is empty. Defaults to Choose an option. */
    placeholder?: string;
/** Hint inside the search input. Defaults to Search options. */
    searchPlaceholder?: string;
/** Message shown when the query matches no choices. */
    emptyText?: string;
/** Validation message shown after a required empty field blocks form submission. */
    requiredMessage?: string;
/** Allows clearing the entire selection. Defaults to true. */
    clearable?: boolean;
/** Whether the searchable Popover is open. */
    open?: boolean;
/** Control height shared with Input and Button; defaults to md. */
    controlSize?: InputSize;
/** Extra classes merged onto the invoking Button. */
    triggerClass?: string;
/** Extra classes merged onto the Popover surface. */
    contentClass?: string;
}
