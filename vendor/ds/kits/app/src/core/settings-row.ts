import { fieldAssociation, type FieldControlProps } from "@reddb-io/design-system/base/recipes";
import type { SettingsRowContract } from "./contracts/settingsrow";

/** The setting name and description belong to fields; actions receive no naming overrides. */
export function settingsRowAssociation(props: SettingsRowContract & { id: string }) {
  const association = fieldAssociation({ id: props.id, help: props.description, error: props.error, required: props.required });
  return { ...association, labelId: `${props.id}-label` };
}
export type { FieldControlProps };
