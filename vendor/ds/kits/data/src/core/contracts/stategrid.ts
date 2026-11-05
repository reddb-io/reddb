import type { StateCell } from "../../operational";
export interface StateGridContract {
  /** Accessible collection name. */
  label: string;
  /** Entities in stable reading order; states are categorical, not numerical intensities. */
  items: readonly StateCell[];
  /** Minimum cell width in characters; the grid wraps within its own container. */
  cellWidth?: number;
  /** Caller-controlled selected entity id. */
  selected?: string | null;
  /** Enables native buttons; false renders a static list. */
  interactive?: boolean;
  /** Reports a requested selection without mutating application state. */
  onselectedchange?: (id: string) => void;
  /** Visible empty-state explanation. */
  emptyLabel?: string;
}
