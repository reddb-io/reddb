export interface TimelineItem {
  /** Stable entry identity. */
  id: string;
  /** Visible entry heading. */
  title: string;
  /** Machine-readable date or time accepted by native `<time>`. */
  datetime: string;
  /** Caller-owned visible rendering of the date or time. */
  time: string;
  /** Supporting event detail. */
  description?: string;
  /** Optional destination for the entry heading. */
  href?: string;
}
