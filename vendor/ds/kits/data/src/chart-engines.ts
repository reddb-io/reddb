// The four LayerChart drawings, each its own dynamic import (and so its own
// chunk, carrying its own LayerChart CSS). Importing this module pulls none of
// them; a chart asks for its engine when its figure nears the viewport.

import type { ComponentProps } from "svelte";
import type { ChartEngine } from "./chart-loader.svelte";

import { chartEngine } from "./chart-loader.svelte";

export const timeSeriesEngine: ChartEngine<ComponentProps<typeof import("./internal/TimeSeriesChart.svelte").default>> = chartEngine(() => import("./internal/TimeSeriesChart.svelte"));
export const histogramEngine: ChartEngine<ComponentProps<typeof import("./internal/HistogramChart.svelte").default>> = chartEngine(() => import("./internal/HistogramChart.svelte"));
export const heatmapEngine: ChartEngine<ComponentProps<typeof import("./internal/HeatmapChart.svelte").default>> = chartEngine(() => import("./internal/HeatmapChart.svelte"));
export const sparklineEngine: ChartEngine<ComponentProps<typeof import("./internal/SparklineChart.svelte").default>> = chartEngine(() => import("./internal/SparklineChart.svelte"));

/** Load every engine now — for tests and for a caller that knows charts are next. */
export function preloadChartEngines(): Promise<void> {
  return Promise.all(
    [timeSeriesEngine, histogramEngine, heatmapEngine, sparklineEngine].map((engine) => engine.load()),
  ).then(() => undefined);
}
