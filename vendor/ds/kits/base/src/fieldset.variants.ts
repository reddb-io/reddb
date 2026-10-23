// Fieldset's association and appearance seams preserve the native group while
// naming only DS roles.
import { tv, type VariantProps } from "tailwind-variants";
import { field } from "./field.variants";

export type { FieldsetControlProps } from "./core/fieldset";

// The group owns its rhythm from the Density layout tier (ADR 0024): the
// legend sits `layout-gap-sm` above the first field, and fields sit
// `layout-gap-sm` apart (8/12/16/24px) — tighter than a Form's rows, so a
// group's fields read as one group inside a Form.
//
// The legend steps to secondary ink when the group is disabled, exactly as a
// Field's label follows its control. A nested group inside a disabled one is
// disabled too (HTML), so its legend steps down with it. Help and error text
// are Field's own, so a group's error reads like every other control's.
export const fieldset = tv({
  slots: {
    root: "group/fieldset grid min-w-0 gap-[var(--reddb-spatial-layout-gap-sm)] border-0 p-0",
    legend: [
      "mb-[var(--reddb-spatial-layout-gap-sm)] text-sm font-medium text-foreground",
      "group-disabled/fieldset:text-ink-muted",
    ].join(" "),
    help: field().help(),
    error: field().error(),
  },
});

export type FieldsetVariants = VariantProps<typeof fieldset>;
