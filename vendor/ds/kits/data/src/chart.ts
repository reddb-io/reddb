/** Pure helpers shared by the Data Kit's LayerChart-powered charts (ADR 0013). */

/** A Series Role position: the categorical hue a chart's marks are drawn in. */
export type ChartSeries = 1 | 2 | 3 | 4 | 5 | 6;

/** Shared Density-relative plot height; `height` remains the explicit pixel override. */
export type ChartSize = "sm" | "md" | "lg";
/** Smooth monotone interpolation, or literal straight segments between observations. */
export type ChartCurve = "smooth" | "linear";
export const CHART_ROWS = { sm: 3, md: 6, lg: 10 } as const;

/** Renders one numeric value as axis or bin text. */
export type ChartNumberFormat = (value: number) => string;

/**
 * A locale-free rendering of `value` with at most `fractionDigits` decimals and
 * no trailing zeros. Locale-free on purpose: a label prerendered on the server
 * hydrates unchanged in any browser, and a caller who wants grouping or units
 * passes its own `format`.
 */
export function formatChartNumber(value: number, fractionDigits = 2): string {
  if (!Number.isFinite(value)) return "";
  const rounded = Number(value.toFixed(Math.max(0, Math.min(20, Math.floor(fractionDigits)))));
  return String(Object.is(rounded, -0) ? 0 : rounded);
}

/**
 * The decimals that tell neighbouring edges `step` apart: one significant digit
 * past the step's own magnitude, so 18 → 23.67 by 5.67 reads `23.7` and a step
 * of 0.25 keeps both its digits.
 */
export function fractionDigitsFor(step: number): number {
  if (!Number.isFinite(step) || step <= 0) return 2;
  return Math.max(0, Math.min(6, 1 - Math.floor(Math.log10(step))));
}

/**
 * The Density rhythm a chart sizes its own geometry by, read off a probe the
 * roles size (as Graph does): `row` is `control-height-md`, `inset` is
 * `inset-sm`. Before anything is measured — prerender, or a document without
 * layout — a chart draws at the comfortable stop's lengths.
 */
export interface ChartRhythm {
  row: number;
  inset: number;
}

export const COMFORTABLE_CHART_RHYTHM: ChartRhythm = Object.freeze({ row: 32, inset: 12 });

/** Follow `probe`'s box and report the rhythm it measures; returns the disconnect. */
export function observeChartRhythm(
  probe: HTMLElement,
  update: (rhythm: ChartRhythm) => void,
): () => void {
  const read = (): void => {
    const box = probe.getBoundingClientRect();
    if (box.height > 0 && box.width > 0) update({ row: box.height, inset: box.width });
  };
  read();
  if (typeof ResizeObserver === "undefined") return () => {};
  const observer = new ResizeObserver(read);
  observer.observe(probe);
  return () => observer.disconnect();
}

// Axis gutters hold text, so they follow the type scale rather than Density:
// tick labels are set at `xs` (12px at the default root size), and a label is
// budgeted at 0.56em per character plus a compact tick gap.
const AXIS_TEXT_PX = 12;
const AXIS_CHARACTER_EM = 0.56;
const AXIS_TICK_ROOM_PX = 6;
const MIN_VALUE_GUTTER_PX = 24;

/** The bottom gutter for one line of category or time labels under the plot. */
export const CATEGORY_AXIS_GUTTER_PX = 28;

/** The width the widest `labels` will take, in px. */
export function axisLabelWidth(labels: readonly string[]): number {
  const characters = labels.reduce((widest, label) => Math.max(widest, label.length), 0);
  return Math.ceil(characters * AXIS_CHARACTER_EM * AXIS_TEXT_PX);
}

/** A left gutter wide enough for the value axis' widest label. */
export function valueAxisGutter(labels: readonly string[]): number {
  return Math.max(MIN_VALUE_GUTTER_PX, axisLabelWidth(labels) + AXIS_TICK_ROOM_PX);
}

/**
 * How many of `labels`, one per band, fit across `plotWidth` without
 * colliding; LayerChart keeps every n-th band label to match.
 */
export function bandLabelCount(labels: readonly string[], plotWidth: number): number {
  const slot = axisLabelWidth(labels) + AXIS_TICK_ROOM_PX;
  return Math.max(1, Math.min(labels.length, Math.floor(plotWidth / slot)));
}

/**
 * Heatmap intensity is stepped, not interpolated: a colour scale cannot
 * interpolate the `var()` references a scheme-aware fill has to be, so each
 * step is its own fill. Step 0 — no value, zero or less — is the quiet
 * `muted` surface; steps 1–4 lay the chart's series colour (`--color-primary`
 * on the figure) over the ground at rising strength, and both ends follow the
 * Color Scheme because neither is ever resolved to a value here.
 */
export const HEATMAP_FILLS = [
  "var(--reddb-color-muted)",
  "color-mix(in srgb, var(--color-primary) 25%, transparent)",
  "color-mix(in srgb, var(--color-primary) 50%, transparent)",
  "color-mix(in srgb, var(--color-primary) 75%, transparent)",
  "var(--color-primary)",
] as const;

export const HEATMAP_STEPS = HEATMAP_FILLS.length - 1;

/** The intensity step (0…HEATMAP_STEPS) of `value` against the largest value shown. */
export function heatmapStep(value: unknown, maximum: number): number {
  if (typeof value !== "number" || !Number.isFinite(value) || value <= 0) return 0;
  if (!Number.isFinite(maximum) || maximum <= 0) return 0;
  return Math.min(HEATMAP_STEPS, Math.max(1, Math.ceil((value / maximum) * HEATMAP_STEPS)));
}

/** Compact trend presentation, shared by both native renderers. */
export type SparklineVariant = "line" | "area" | "bar";
