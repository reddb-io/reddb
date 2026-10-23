import { tv, type VariantProps } from "tailwind-variants";

export const INDICATOR_POSITIONS = [
  "top-start",
  "top-center",
  "top-end",
  "middle-start",
  "middle-end",
  "bottom-start",
  "bottom-center",
  "bottom-end",
] as const;
export type IndicatorPosition = (typeof INDICATOR_POSITIONS)[number];

const POSITION: Record<IndicatorPosition, { marker: string }> = {
  "top-start": { marker: "start-0 top-0 -translate-x-1/2 -translate-y-1/2" },
  "top-center": { marker: "start-1/2 top-0 -translate-x-1/2 -translate-y-1/2" },
  "top-end": { marker: "end-0 top-0 translate-x-1/2 -translate-y-1/2" },
  "middle-start": { marker: "start-0 top-1/2 -translate-x-1/2 -translate-y-1/2" },
  "middle-end": { marker: "end-0 top-1/2 translate-x-1/2 -translate-y-1/2" },
  "bottom-start": { marker: "bottom-0 start-0 -translate-x-1/2 translate-y-1/2" },
  "bottom-center": { marker: "bottom-0 start-1/2 -translate-x-1/2 translate-y-1/2" },
  "bottom-end": { marker: "bottom-0 end-0 translate-x-1/2 translate-y-1/2" },
};

export const indicator = tv({
  slots: {
    root: "relative inline-flex",
    marker: "absolute z-10 inline-flex",
  },
  variants: { position: POSITION },
  defaultVariants: { position: "top-end" },
});

export type IndicatorVariants = VariantProps<typeof indicator>;
