import { tv } from "tailwind-variants";
import { slider } from "@reddb-io/design-system/base/recipes";

/** Native chart controls share Base geometry and Density's pointer floor. */
export const visualizationControls = tv({
  slots: {
    root: "mt-[var(--reddb-spatial-gap-md)] flex min-w-0 flex-wrap items-center gap-[var(--reddb-spatial-gap-md)] text-xs",
    field:
      "flex min-w-0 max-w-full flex-[1_1_24ch] items-center gap-[var(--reddb-spatial-gap-sm)]",
    range: slider().control({ class: "min-w-0 flex-1" }),
    action:
      "inline-flex min-h-[var(--reddb-spatial-control-height-sm)] min-w-[var(--reddb-spatial-target-min)] max-w-full items-center [overflow-wrap:anywhere]",
    summary: "min-h-[var(--reddb-spatial-control-height-sm)] content-center",
  },
});
