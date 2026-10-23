import { fieldAssociation, type FieldControlProps } from "./field";

/** A group association leaves every native control's id and requirement intact. */
export type FieldsetControlProps = Omit<FieldControlProps, "id" | "required">;

/** Group help/error links use the same normalization as a single Field. */
export function fieldsetAssociation(options: { id: string; help?: string; error?: string }) {
  const { control: { id, required, ...control }, helpId, errorId } = fieldAssociation(options);
  return { control, helpId, errorId };
}
