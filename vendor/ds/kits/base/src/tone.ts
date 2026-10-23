// The one semantic vocabulary every Kit speaks (ADR 0026).
//
// A component that expresses meaning takes `tone`, and every value resolves to
// the same Feedback Role in every component: `info` is feedback-info on a Card
// exactly as on an Alert, a Badge or a Button. How strongly a tone is drawn —
// filled, tinted, outlined or plain — is a separate emphasis axis owned by each
// component (Button's `variant`, Badge's `variant`), never a tone value, so the
// accent (`primary`, `brand`) is not a tone.
//
// Base declares the vocabulary once; child Kits import it from here rather
// than restating it, so the list cannot drift between components.

/** The tone vocabulary: neutral plus the four Feedback Roles. */
export const TONES = ["neutral", "info", "success", "warning", "danger"] as const;
export type Tone = (typeof TONES)[number];

/** The tones that name a Feedback Role (ADR 0009) — every tone except neutral. */
export const FEEDBACK_TONES = ["info", "success", "warning", "danger"] as const satisfies readonly Tone[];
export type FeedbackTone = (typeof FEEDBACK_TONES)[number];

/** Whether a value belongs to the tone vocabulary. */
export function isTone(value: unknown): value is Tone {
  return typeof value === "string" && (TONES as readonly string[]).includes(value);
}
