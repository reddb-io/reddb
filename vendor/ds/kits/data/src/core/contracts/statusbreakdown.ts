import type { StatusCount } from "../status-breakdown";
export interface StatusBreakdownContract {
  /** Visible and accessible name of the state summary. */
  label: string;
  /** Independent state counts; progress and readiness remain application-owned. */
  items: readonly StatusCount[];
  /** Caller-controlled state filter; null displays the complete collection. */
  selected?: string | null;
  /** Enables state-filter buttons; false renders a static list. */
  interactive?: boolean;
  /** Requests a state filter; activating the current state requests null. */
  onselectedchange?: (id: string | null) => void;
  /** Explanation shown when the total count is zero. */
  emptyLabel?: string;
}
