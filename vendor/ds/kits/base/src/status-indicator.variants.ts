import { tv, type VariantProps } from "tailwind-variants";
import { TONES, type Tone } from "./tone";

/** The tones a StatusIndicator accepts: the shared vocabulary (ADR 0026). */
export const STATUS_INDICATOR_TONES = TONES;
export type StatusIndicatorTone = Tone;
/** @deprecated StatusIndicator's meaning is `tone` (ADR 0026); use `STATUS_INDICATOR_TONES`. Removed next release. */
export const STATUS_INDICATOR_STATUSES = TONES;
/** @deprecated StatusIndicator's meaning is `tone` (ADR 0026); use `StatusIndicatorTone`. Removed next release. */
export type StatusIndicatorStatus = Tone;

// Every mark is a non-text graphic that, with the label hidden, stands in for
// it, so each fill holds 3:1 on every ground (WCAG 1.4.11). Neutral follows the
// feedback anatomy — the edge stop rings the text-grade fill — in the neutral
// roles: `muted` is a surface role at 8% ink (ADR 0019), 1.19:1 as a mark.
const TONE: Record<Tone, { mark: string }> = {
  neutral: { mark: "border-control-edge bg-ink-muted" },
  info: {
    mark:
      "border-[var(--reddb-color-feedback-info-border)] bg-[var(--reddb-color-feedback-info-foreground)]",
  },
  success: {
    mark:
      "border-[var(--reddb-color-feedback-success-border)] bg-[var(--reddb-color-feedback-success-foreground)]",
  },
  warning: {
    mark:
      "border-[var(--reddb-color-feedback-warning-border)] bg-[var(--reddb-color-feedback-warning-foreground)]",
  },
  danger: {
    mark:
      "border-[var(--reddb-color-feedback-danger-border)] bg-[var(--reddb-color-feedback-danger-foreground)]",
  },
};

export const statusIndicator = tv({
  slots: {
    root: "inline-flex min-w-0 max-w-full items-center gap-[var(--reddb-spatial-gap-sm)] text-sm text-foreground",
    mark: "size-2 shrink-0 rounded-full border",
    label: "min-w-0 [overflow-wrap:anywhere]",
  },
  variants: {
    tone: TONE,
    labelled: {
      true: { label: "" },
      false: { label: "sr-only" },
    },
  },
  defaultVariants: { tone: "neutral", labelled: true },
});

export type StatusIndicatorVariants = VariantProps<typeof statusIndicator>;
