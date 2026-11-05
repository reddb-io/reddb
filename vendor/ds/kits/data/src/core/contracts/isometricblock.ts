import type { VisualizationSeries } from "../../visualization";
export interface IsometricBlockContract {
  /** Accessible description of the solid volume. */
  label: string;
  /** Width in logical drawing units, not CSS pixels. */
  width?: number;
  /** Depth in logical drawing units. */
  depth?: number;
  /** Solid height in logical drawing units. */
  height?: number;
  /** Stable categorical Series Role. */
  series?: VisualizationSeries;
}
