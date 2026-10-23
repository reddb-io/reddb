import type { CodeFieldGrammar, CodeFieldSuggestion, VariableResolution } from "../../code-field";
/** Portable CodeField properties shared by native adapters. */
export interface CodeFieldContract {
    /** Visible name for the code control. */
    label: string;
    /**
         * The field's text. Bindable: every edit and accepted completion writes the new text back;
         * empty when omitted.
         */
    value?: string;
    /** Caller-supplied syntax and completion behavior. */
    grammar?: CodeFieldGrammar;
    /** Names resolved by the reference grammar when no custom grammar is supplied. */
    resolved?: VariableResolution;
    /**
         * Completion candidates for the listbox under the field, as plain strings or objects with
         * `value`, `label` and `disabled`. Only those whose `value` starts with the text being
         * completed (ignoring case) are listed; omitted, no list opens.
         */
    suggestions?: readonly (CodeFieldSuggestion | string)[];
    /** Render a multiline textarea with line numbers. */
    gutter?: boolean;
    /**
         * Visible line count of the textarea when `gutter` is on; ignored in single-line mode. Defaults
         * to `5`.
         */
    rows?: number;
    /**
         * `id` of the native control, which the visible label points at. Defaults to a generated id
         * unique to the instance.
         */
    id?: string;
    /**
         * `name` of the native control, so the text is submitted with an enclosing form. Omitted, the
         * control has no form name.
         */
    name?: string;
    /** Hint text shown inside the control while it is empty. */
    placeholder?: string;
    /**
         * Supporting text rendered below the field and linked to the control as its accessible
         * description. Omitted, nothing is rendered.
         */
    help?: string;
    /**
         * Marks the native control `required` and adds a decorative asterisk after the label. Defaults
         * to `false`.
         */
    required?: boolean;
    /**
         * Disables the native control: no edits or focus, the control is dimmed and no completions
         * open. Defaults to `false`.
         */
    disabled?: boolean;
    /**
         * Extra classes, merged onto the native input or textarea itself rather than the outer wrapper.
         */
    inputClass?: string;
}
