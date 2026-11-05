// Form's public scaffold seam. The Form owns the rhythm between its own rows —
// fields, groups and the action row — and draws it from the Density layout
// tier (ADR 0024): `layout-gap-md`, 16/20/24/40px from tiny to spacious. A
// Field's label sits `gap-sm` from its control, never more than a quarter of
// that, so each label reads with its own control. Local arrangement (columns,
// an inline action row) stays extensible through `class`.
import { tv, type VariantProps } from "tailwind-variants";

export const form = tv({
  base: "grid gap-[var(--reddb-spatial-layout-gap-md)]",
});

export type FormVariants = VariantProps<typeof form>;
