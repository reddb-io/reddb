/** Portable field semantics shared by the native InlineEdit adapters. */
export interface InlineEditContract {
  /** Visible field name, also used to name its edit and input controls. */
  label: string;
  /** Read layout: compact text by default, or a full-width saved value that becomes an input only while editing. */
  presentation?: "text" | "input";
  /** Whether the edit action is always visible or revealed by hover/focus. Touch always shows it. */
  editVisibility?: "always" | "hover";
  /** Caller-supplied provenance, such as API, imported file or organization defaults. Never inferred. */
  sourceLabel?: string;
  /** Visible committed-state label in input presentation. Defaults to Saved. */
  savedLabel?: string;
  /** Native string input type while editing. Password values are masked in the read view. Defaults to text. */
  inputType?: "text" | "password" | "email" | "url" | "tel";
  /** Committed text. Draft keystrokes never change this value. */
  value: string;
  /** Whether the draft editor is open; caller-owned in controlled React use. */
  editing?: boolean;
  /** Initial uncontrolled editor state; later changes do not replace the user's choice. */
  defaultEditing?: boolean;
  /** Optional application validation; return an error to retain the draft. */
  validate?: (value: string) => string | undefined;
  /** Prevents opening or submitting the editor. */
  disabled?: boolean;
  /** Requires at least one non-whitespace character. Defaults to true. */
  required?: boolean;
  /** Optional maximum number of characters accepted by the native input. */
  maxlength?: number;
  /** Visible text when the committed value is empty. */
  emptyLabel?: string;
  /** Label of the action that opens the editor. */
  editLabel?: string;
  /** Label of the action that persists the draft. */
  saveLabel?: string;
  /** Label of the action that discards the draft. */
  cancelLabel?: string;
  /** Validation message for a required empty draft. */
  requiredMessage?: string;
  /** Message announced when persistence rejects. */
  saveErrorMessage?: string;
}

export interface InlineEditState { draft: string; error: string; saving: boolean }
export interface InlineEditSaveOptions {
  required: boolean;
  requiredMessage: string;
  saveErrorMessage: string;
  validate?: (value: string) => string | undefined;
  confirm?: (value: string) => void | Promise<void>;
}
export type InlineEditResult = { status: "committed"; value: string } | { status: "invalid" | "failed" | "busy" | "stale" };

/** A draft transaction; invalidated confirmations cannot commit a later editing session. */
export function createInlineEditSession(value: string, changed: (state: InlineEditState) => void) {
  let state: InlineEditState = { draft: value, error: "", saving: false };
  let generation = 0;
  const publish = (next: InlineEditState) => { state = next; changed(state); };
  return {
    get state() { return state; },
    invalidate() { generation++; },
    reset(value: string) { generation++; publish({ draft: value, error: "", saving: false }); },
    change(draft: string) { if (!state.saving) publish({ ...state, draft, error: "" }); },
    async save(options: InlineEditSaveOptions): Promise<InlineEditResult> {
      if (state.saving) return { status: "busy" };
      const candidate = state.draft;
      const error = options.required && !candidate.trim() ? options.requiredMessage : options.validate?.(candidate) ?? "";
      if (error) { publish({ ...state, error }); return { status: "invalid" }; }
      const ticket = ++generation;
      publish({ ...state, saving: true, error: "" });
      try { await options.confirm?.(candidate); }
      catch {
        if (ticket !== generation) return { status: "stale" };
        publish({ ...state, error: options.saveErrorMessage, saving: false });
        return { status: "failed" };
      }
      if (ticket !== generation) return { status: "stale" };
      publish({ ...state, saving: false });
      return { status: "committed", value: candidate };
    },
  };
}

/** Read mode is selectable text; password values never enter the read DOM. */
export function inlineEditReadValue(value: string, inputType: InlineEditContract["inputType"], emptyLabel: string): string {
  return value ? (inputType === "password" ? "••••••••" : value) : emptyLabel;
}
