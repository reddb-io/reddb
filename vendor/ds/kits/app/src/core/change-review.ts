export type ReviewState = "pending" | "applying" | "applied" | "failed";
export interface ReviewChange {
  /** Unique stable identity; never a row position. */
  id: string;
  /** Human-readable changed entity or property. */
  label: string;
  /** Caller-formatted previous value; never executed. */
  before: string;
  /** Caller-formatted proposed value; never executed. */
  after: string;
  /** Observed result; omitted means pending. */
  state?: ReviewState;
  /** Caller-safe failure explanation. */
  error?: string;
  /** Optional context for the change. */
  description?: string;
}
export function reviewChanges(items: readonly ReviewChange[]) {
  const ids = new Set<string>();
  for (const item of items) {
    if (!item.id.trim() || ids.has(item.id))
      throw new Error("ChangeReview requires unique non-empty ids.");
    if (
      item.state &&
      !["pending", "applying", "applied", "failed"].includes(item.state)
    )
      throw new Error("Unknown review state.");
    ids.add(item.id);
  }
  return {
    items,
    actionable: items
      .filter((item) => item.state !== "applied")
      .map((item) => item.id),
    applying: items.some((item) => item.state === "applying"),
  };
}
