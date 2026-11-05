import type { KpiPolarity } from "../kpi-card";
export interface StripMetric {
  /** Stable metric identity. */
  id: string;
  /** Caller-owned metric name. */
  label: string;
  /** Caller-formatted headline reading. */
  value: string | number;
  /** Supporting context. */
  description?: string;
  /** Optional delta against the comparison period. */
  delta?: number;
  /** Meaning of increasing/decreasing values. */
  polarity?: KpiPolarity;
  /** Optional trend samples, never fabricated by the strip. */
  values?: readonly number[];
  /** Shape of the miniature trend. */
  variant?: "line" | "area" | "bar";
}
export interface MetricStripContract {
  /** Accessible name of the related metric collection. */
  label: string;
  /** Metrics in reading order, with stable ids. */
  items: readonly StripMetric[];
}
