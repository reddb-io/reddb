/** A finite, ordered range shared by every value-based feedback capability. */
export interface NormalizedRange {
  min: number;
  max: number;
  value: number;
  /** Current position in the range, from zero through one hundred. */
  percent: number;
}

/** True only when a progress value can be announced as determinate. */
export function isDeterminate(value: number | null | undefined): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

/**
 * Make native range input safe for rendering and accessibility attributes.
 * A reversed or collapsed range becomes one unit wide; a value is clamped.
 */
export function normalizeRange(value: number, min = 0, max = 100): NormalizedRange {
  const safeMin = Number.isFinite(min) ? min : 0;
  const candidateMax = Number.isFinite(max) ? max : safeMin + 100;
  const safeMax = candidateMax > safeMin ? candidateMax : safeMin + 1;
  const candidateValue = Number.isFinite(value) ? value : safeMin;
  const safeValue = Math.min(safeMax, Math.max(safeMin, candidateValue));

  return {
    min: safeMin,
    max: safeMax,
    value: safeValue,
    percent: ((safeValue - safeMin) / (safeMax - safeMin)) * 100,
  };
}

/** The default audible and visible rendering of a normalized progress value. */
export function formatPercent(range: NormalizedRange): string {
  return `${Math.round(range.percent)}%`;
}
