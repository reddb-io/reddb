/** Shared, browser-safe contracts. No renderer, layout engine or component registry. */
export type VisualizationSize = "sm" | "md" | "lg";
export type VisualizationSeries = 1 | 2 | 3 | 4 | 5 | 6;
export interface VisualizationItem {
  id: string;
  label: string;
  description?: string;
  series?: VisualizationSeries;
  href?: string;
}
export interface VisualizationRow {
  id: string;
  label: string;
  value: string;
  description?: string;
  href?: string;
}
export const ink = "var(--reddb-color-foreground)";
export const mutedInk = "var(--reddb-color-ink-muted)";
export const edge = "var(--reddb-color-muted)";
export const surface = "var(--reddb-color-elevation-raised-surface)";
export function seriesColor(series: VisualizationSeries = 1): string {
  return `var(--reddb-color-series-${series})`;
}
export function tint(color: string, amount = 15): string {
  return `color-mix(in srgb, ${color} ${amount}%, var(--reddb-color-background))`;
}
export function unique(items: readonly { id: string }[], subject: string): void {
  const ids = new Set<string>();
  for (const item of items) {
    if (!item.id || ids.has(item.id)) throw new Error(`${subject}: duplicate or empty id ${item.id}`);
    ids.add(item.id);
  }
}
export function finite(value: number, subject: string): number {
  if (!Number.isFinite(value)) throw new Error(`${subject}: expected a finite number`);
  return value;
}
export function domain(values: readonly number[], explicit?: readonly [number, number]): [number, number] {
  if (explicit) {
    if (!explicit.every(Number.isFinite) || explicit[0] >= explicit[1]) throw new Error("Domain must be finite and increasing");
    return [explicit[0], explicit[1]];
  }
  if (!values.length) return [0, 1];
  let low = Infinity, high = -Infinity;
  for (const value of values) { finite(value, "Domain"); low = Math.min(low, value); high = Math.max(high, value); }
  if (low === high) { const room = Math.abs(low) * 0.1 || 1; return [low - room, high + room]; }
  return [low, high];
}
export function project(value: number, range: readonly [number, number], start: number, end: number): number {
  return start + (value - range[0]) / (range[1] - range[0]) * (end - start);
}
export function ticks(range: readonly [number, number], count = 4): number[] {
  return Array.from({ length: count + 1 }, (_, i) => range[0] + (range[1] - range[0]) * i / count);
}
export function number(value: number): string {
  return new Intl.NumberFormat("en", { notation: Math.abs(value) >= 10_000 ? "compact" : "standard", maximumFractionDigits: 2 }).format(value);
}
/** Limit a visible label; its complete text remains in the data table and SVG title. */
export function short(value: string, length = 24): string {
  return value.length > length ? `${value.slice(0, length - 1)}…` : value;
}
