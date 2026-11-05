import type { LogEvent } from "../../operational";
export interface EventLogContract {
  /** Accessible name of this event history. */
  label: string;
  /** Caller-ordered events with stable ids and machine-readable timestamps. */
  events: readonly LogEvent[];
  /** Retained tail before filtering; clamped to 1–10000. */
  limit?: number;
  /** Case-insensitive source/message query, supplied by the caller. */
  query?: string;
  /** Follow new entries; wheel, touch and history navigation suspend following. */
  following?: boolean;
  /** Reports suspension or resume intent; no live stream is owned here. */
  onfollowingchange?: (following: boolean) => void;
  /** Visible empty-filter explanation. */
  emptyLabel?: string;
}
