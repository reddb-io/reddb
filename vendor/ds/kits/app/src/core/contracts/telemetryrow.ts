import type { Tone } from "@reddb-io/design-system/base";
export interface TelemetryRowContract {
  /** Name of the entity or resource. */
  label: string;
  /** Bounded resource reading; distinct from the operational state. */
  value: number;
  /** Resource capacity. */
  max?: number;
  /** Explicit operational status. */
  stateLabel?: string;
  /** Independent condition such as locked, equipped or maintenance. */
  condition?: string;
  /** Supporting context or unit. */
  description?: string;
  /** Meaning of the operational status. */
  tone?: Tone;
}
