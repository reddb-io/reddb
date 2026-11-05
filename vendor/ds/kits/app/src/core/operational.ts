import type { Tone } from "@reddb-io/design-system/base";
export interface PropertyItem {
  /** Stable name of the property. */ label: string;
  /** Caller-formatted reading, never an editor. */ value: string | number;
}
export interface PhaseGate {
  /** Stable identity. */ id: string;
  /** Visible gate name. */ label: string;
  /** Explicit state; completion is not inferred from work progress. */ state:
    "pending" | "running" | "passed" | "failed";
  /** Optional context or approval requirement. */ description?: string;
}
export const GATE_LABELS = {
  pending: "Pending",
  running: "Running",
  passed: "Passed",
  failed: "Failed",
} as const;
export const GATE_TONES: Record<PhaseGate["state"], Tone> = {
  pending: "neutral",
  running: "info",
  passed: "success",
  failed: "danger",
};
export function validateGates(gates: readonly PhaseGate[]) {
  const ids = new Set<string>();
  for (const gate of gates) {
    if (!gate.id || ids.has(gate.id) || !Object.hasOwn(GATE_LABELS, gate.state))
      throw new Error(
        "PhaseSummary: gates require unique ids and known states",
      );
    ids.add(gate.id);
  }
  return gates;
}

export const PLAYBACK_SPEEDS = [
  { value: "0.5", label: "0.5×" },
  { value: "1", label: "1×" },
  { value: "2", label: "2×" },
  { value: "4", label: "4×" },
] as const;
