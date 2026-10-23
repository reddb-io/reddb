import type { Tone } from "@reddb-io/design-system/base";

export interface StatusCount {
  /** Stable state identity, independent of its localized label. */
  id: string;
  /** Visible state label. */
  label: string;
  /** Exact, non-negative integer count. */
  count: number;
  /** Semantic state tone, never inferred from the count. */
  tone?: Tone;
}

/** Resolves independent categorical counts without inventing an overall readiness state. */
export function statusCounts(items: readonly StatusCount[]) {
  const ids = new Set<string>();
  let total = 0;
  for (const item of items) {
    if (!item.id || ids.has(item.id))
      throw new Error("StatusBreakdown: ids must be non-empty and unique.");
    if (!Number.isSafeInteger(item.count) || item.count < 0)
      throw new Error(
        "StatusBreakdown: counts must be non-negative safe integers.",
      );
    ids.add(item.id);
    total += item.count;
    if (!Number.isSafeInteger(total))
      throw new Error("StatusBreakdown: total exceeds safe integer range.");
  }
  return {
    total,
    items: items.map((item) => ({
      ...item,
      fraction: total ? item.count / total : 0,
      paint:
        item.tone && item.tone !== "neutral"
          ? `var(--reddb-color-feedback-${item.tone}-foreground)`
          : "var(--reddb-color-ink-muted)",
    })),
  };
}
