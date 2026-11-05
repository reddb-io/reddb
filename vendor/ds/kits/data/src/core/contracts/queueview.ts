import type { StateCell } from "../../operational";
export interface QueueSlot {
  /** Stable slot identity. */
  id: string;
  /** Name of the position. */
  label: string;
  /** Actual occupant; omit for an empty slot. */
  item?: StateCell;
}
export interface QueueViewContract {
  /** Accessible name of the ordered slot collection. */
  label: string;
  /** Actual slots in queue order; the component never schedules or reorders them. */
  slots: readonly QueueSlot[];
  /** Caller-controlled selected occupant id. */
  selected?: string | null;
  /** Whether occupied slots offer selection. */
  interactive?: boolean;
  /** Reports an occupied entity id for application-owned inspection. */
  onselectedchange?: (id: string) => void;
  /** Text for an unoccupied slot. */
  emptyLabel?: string;
}
