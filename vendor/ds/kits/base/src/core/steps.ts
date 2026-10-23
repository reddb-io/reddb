export type StepState = "complete" | "current" | "upcoming";

export interface StepItem {
  /** Stable step identity. */
  id: string;
  /** Visible step name. */
  label: string;
  /** Optional destination for a non-current step. */
  href?: string;
  /** Progress state, independent of appearance axes. */
  state: StepState;
}
