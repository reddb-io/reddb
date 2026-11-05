import { tv, type VariantProps } from "tailwind-variants";
import { MEASURE_TONE_FILL } from "./meter.variants";

export const progress = tv({
  slots: {
    root: "w-full",
    summary: "mb-[var(--reddb-spatial-gap-sm)] flex items-center justify-between gap-[var(--reddb-spatial-gap-md)]",
    label: "text-sm text-foreground",
    value: "text-sm font-medium text-foreground",
    track:
      "h-[var(--reddb-spatial-gap-md)] w-full overflow-hidden rounded-full bg-muted",
    indicator: "h-full rounded-full transition-[width] motion-reduce:transition-none",
  },
  variants: {
    // What the work's progress means (ADR 0026): ink unless a tone is given.
    tone: {
      neutral: { indicator: MEASURE_TONE_FILL.neutral },
      info: { indicator: MEASURE_TONE_FILL.info },
      success: { indicator: MEASURE_TONE_FILL.success },
      warning: { indicator: MEASURE_TONE_FILL.warning },
      danger: { indicator: MEASURE_TONE_FILL.danger },
    },
    indeterminate: {
      true: { indicator: "w-1/3 motion-safe:animate-pulse" },
      false: { indicator: "" },
    },
  },
  defaultVariants: { tone: "neutral", indeterminate: false },
});

export type ProgressVariants = VariantProps<typeof progress>;
