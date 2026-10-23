import type { ReviewChange } from "../change-review";
/** Review of caller-owned changes, with explicit request-only batch actions. */
export interface ChangeReviewContract {
  /** Visible and accessible review name. */
  title: string;
  /** Explicit proposed values and observed per-item results. */
  items: readonly ReviewChange[];
  /** Disable actions during application-owned work. */
  applying?: boolean;
  /** Explain why changes cannot be applied. */
  blockedReason?: string;
  /** Requests application of pending/failed ids; no mutation or success is inferred. */
  onapplyrequest?: (ids: readonly string[]) => void;
  /** Requests discarding one or all pending/failed ids. */
  ondiscardrequest?: (ids: readonly string[]) => void;
  /** Name for previous values. */
  beforeLabel?: string;
  /** Name for proposed values. */
  afterLabel?: string;
  /** Batch apply action label. */
  applyLabel?: string;
  /** Batch discard action label. */
  discardLabel?: string;
  /** Per-item discard action prefix. */
  discardItemLabel?: string;
  /** Empty review explanation. */
  emptyLabel?: string;
  /** Localized pending state. */
  pendingLabel?: string;
  /** Localized applying state. */
  applyingLabel?: string;
  /** Localized applied state. */
  appliedLabel?: string;
  /** Localized failed state. */
  failedLabel?: string;
  /** Outline depth of the visible review title. */
  headingLevel?: 1 | 2 | 3 | 4 | 5 | 6;
}
