import type { ChartSeries } from "../../chart";
import type { ChartLoading } from "../../chart-loading";
import type { HeatmapPoint } from "../heatmap";
/** Portable Heatmap properties shared by native adapters. */
export interface HeatmapContract {
    /**
         * Dated values as `{ date, value }` points. Points on the same local day add up, and each day's
         * cell takes a step on the intensity ramp relative to the busiest day in range; points outside
         * `start` and `end`, or with a non-finite value, are ignored.
         */
    data: readonly HeatmapPoint[];
    /** Inclusive first calendar day. */
    start: Date;
    /** Exclusive end of the calendar range. */
    end: Date;
    /** Accessible description of the represented intensity. */
    label?: string;
    /** The Series Role (1–6) the intensity ramp is drawn in. Never the Brand red. */
    series?: ChartSeries;
    /**
         * Fixed drawing width in px. Omitted, the calendar fills its figure's
         * content width and follows it on resize (640 until the figure is measured);
         * the container sets that width, and the chart never widens its container.
         */
    width?: number;
    /** Fixed drawing height in px. Omitted, five rows of the Density's control height. */
    height?: number;
    /**
         * When LayerChart loads: `lazy` (default) once the figure nears the viewport, `eager` as soon as
         * it mounts. Until then — and in the server render — the figure holds a placeholder of the
         * chart's own Density-sized height under its accessible name.
         */
    loading?: ChartLoading;
}
