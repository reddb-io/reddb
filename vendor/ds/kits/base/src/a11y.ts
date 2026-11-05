// Svelte owns only context; DOM diagnostics are shared with native React.
import { getContext, hasContext, setContext } from "svelte";
export { warnA11y, hasAccessibleName, checkFieldName, checkButtonName, FIELD_NAME_FIX } from "./core/a11y";

const FIELD_CONTEXT = Symbol("reddb.field");

/** What a Field tells the control rendered inside it. */
export interface FieldContext {
  /** The Field's visible label, which names the control through `<label for>`. */
  readonly label: string;
}

/** Called by Field so the control inside it knows its name is owned. */
export function setFieldContext(context: FieldContext): void {
  setContext(FIELD_CONTEXT, context);
}

/** The enclosing Field's context, or `undefined` outside one. */
export function getFieldContext(): FieldContext | undefined {
  return hasContext(FIELD_CONTEXT) ? getContext<FieldContext>(FIELD_CONTEXT) : undefined;
}
