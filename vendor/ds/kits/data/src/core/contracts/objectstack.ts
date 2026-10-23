import type { VisualizationSeries } from "../../visualization";
export interface ObjectStackContract {
  /** Name of the represented collection. */
  label: string;
  /** Exact nonnegative integer count; never inferred from drawn representatives. */
  count: number;
  /** Bound on individually drawn representatives, clamped to 1–24. */
  maxVisible?: number;
  /** Stable categorical Series Role. */
  series?: VisualizationSeries;
}
