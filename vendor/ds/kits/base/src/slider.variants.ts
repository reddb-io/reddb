// Slider's token-only appearance seam; native range semantics stay in the component.
import { tv, type VariantProps } from "tailwind-variants";

export const slider = tv({
  slots: {
    root: "grid min-w-0 gap-[var(--reddb-spatial-gap-sm)]",
    control: [
      "h-[var(--reddb-spatial-control-height-sm)] w-full cursor-pointer accent-primary",
      "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus",
      "disabled:cursor-not-allowed disabled:opacity-50",
    ].join(" "),
    output: "text-sm tabular-nums text-ink-muted",
  },
});

export type SliderVariants = VariantProps<typeof slider>;
