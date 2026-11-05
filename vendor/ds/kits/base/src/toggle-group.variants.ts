// ToggleGroup's public option contract and token-only group appearance seam.
import { tv, type VariantProps } from "tailwind-variants";

export interface ToggleGroupOption {
  value: string;
  label: string;
  disabled?: boolean;
}

export const toggleGroup = tv({
  slots: {
    root: "min-w-0",
    // The standard segmented control (maintainer decision, 2026-10-06): the
    // group is exactly one Button tall — `control-height-md`, its border
    // included — and its segments sit flush, split by control-edge dividers.
    // This deliberately drops the inset the group carried since issue 423.
    list: [
      "box-border inline-flex h-[var(--reddb-spatial-control-height-md)] flex-nowrap items-stretch gap-0 rounded-md",
      "border border-control-edge",
    ].join(" "),
    // Every option carries the 2px bottom edge the pressed indicator paints,
    // transparent until pressed, so choosing an option moves nothing. Each
    // option but the last draws the divider on its inline end; the outer
    // segments follow the group's rounding so a pressed fill stays inside it.
    option: [
      "h-auto rounded-none border-0 border-b-2 border-e border-transparent border-e-control-edge last:border-e-0",
      "first:rounded-s-md last:rounded-e-md",
      "px-[var(--reddb-spatial-inset-md)] leading-normal",
    ].join(" "),
  },
  variants: {
    // An error edges the whole group in the danger role, as an invalid Input
    // is edged; the separators between options keep the control edge.
    invalid: { true: { list: "border-feedback-danger-border" } },
  },
});

export type ToggleGroupVariants = VariantProps<typeof toggleGroup>;
