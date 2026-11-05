// Alert's public appearance seam consumes DS-owned Feedback Roles only. The
// Theme may remap their Brand materials without changing these names or the
// component contract (ADR 0009). Meaning is `tone`, the shared vocabulary
// (ADR 0026): each Feedback Role draws its copy on its tinted surface (ADR
// 0023), and neutral is ink on the muted surface.
import { tv, type VariantProps } from "tailwind-variants";
import { FEEDBACK_TONES, TONES, type FeedbackTone, type Tone } from "./tone";

/** Every tone an Alert accepts: the shared vocabulary. */
export const ALERT_TONES = TONES;
export type AlertTone = Tone;

/** @deprecated Alert's meaning is `tone` (ADR 0026); use `FEEDBACK_TONES` or `ALERT_TONES`. Removed next release. */
export const ALERT_FEEDBACK_ROLES = FEEDBACK_TONES;
/** @deprecated Alert's meaning is `tone` (ADR 0026); use `AlertTone`. Removed next release. */
export type AlertFeedbackRole = FeedbackTone;

const TONE: Record<Tone, string> = {
  neutral: "border-transparent bg-muted text-foreground",
  info: "border-feedback-info-border bg-feedback-info-surface text-feedback-info-foreground",
  success: "border-feedback-success-border bg-feedback-success-surface text-feedback-success-foreground",
  warning: "border-feedback-warning-border bg-feedback-warning-surface text-feedback-warning-foreground",
  danger: "border-feedback-danger-border bg-feedback-danger-surface text-feedback-danger-foreground",
};

export const alert = tv({
  base: [
    "rounded-md border",
    "px-[var(--reddb-spatial-inset-md)] py-[var(--reddb-spatial-inset-sm)]",
    "text-sm",
  ].join(" "),
  variants: { tone: TONE },
  defaultVariants: { tone: "neutral" },
});

export type AlertVariants = VariantProps<typeof alert>;
