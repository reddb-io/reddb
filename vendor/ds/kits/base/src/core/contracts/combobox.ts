import type { ComboboxOption, ComboboxSize } from "../../combobox.variants";

/** Canonical portable Combobox properties, shared by native adapters. */
export interface ComboboxContract {
/** Visible Field label for the typeahead input. */
    label: string;
/** Caller-owned selectable values and their searchable labels. */
    options: readonly ComboboxOption[];
/** Native form name applied to the selected value. */
    name?: string;
/** Selected option value. */
    value?: string;
/** Current text in the typeahead input. */
    inputValue?: string;
/** Supporting text announced with the input. */
    help?: string;
/** Current validation error announced with the input. */
    error?: string;
/** Marks the input as required: the label shows it and the native control enforces it. */
    required?: boolean;
/** Disables the input and the listbox, so nothing can be typed or chosen. */
    disabled?: boolean;
/** Hint text shown in the input while it is empty. */
    placeholder?: string;
/** Whether the anchored listbox is open. */
    open?: boolean;
/** The control height step, matching Button's `size` (default `md`). */
    controlSize?: ComboboxSize;
/** Extra classes merged onto the typeahead input. */
    inputClass?: string;
/** Extra classes merged onto the anchored listbox surface. */
    contentClass?: string;
/** Extra classes merged onto every option row. */
    itemClass?: string;
}
