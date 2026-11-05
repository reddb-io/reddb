import type { PhaseGate } from "../operational";
export interface PhaseSummaryContract {
  /** Name of this independently verified phase collection. */
  label: string;
  /** Caller-owned execution, verification and approval gates. */
  gates: readonly PhaseGate[];
  /** Overall reading supplied by the caller; never inferred from a percentage. */
  description?: string;
}
