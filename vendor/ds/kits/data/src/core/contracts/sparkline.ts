import type { ChartCurve, ChartSeries, SparklineVariant } from "../../chart";
import type { ChartLoading } from "../../chart-loading";
/** Portable Sparkline properties shared by native adapters. */
export interface SparklineContract {
    /**
         * The observations to plot, in order from left to right; the position in the array is
         * the x position.
         */
    values: readonly number[];
    /** `line` (default) shows the trend; `area` fills it to zero; `bar` draws one vertical column per observation, diverging around zero for negative values. */
    variant?: SparklineVariant;
    /** Accessible description of the trend. */
    label?: string;
    /** The Series Role (1–6) the trend is drawn in. Never the Brand red. */
    series?: ChartSeries;
    /** `smooth` (default) uses monotone interpolation through the observations without local overshoot; `linear` joins them with straight segments. Applies to line and area; columns are unchanged. Values and statistics are unchanged. */
    curve?: ChartCurve;
    /** Drawing width in px. Omitted, fills its container; an explicit width keeps a fixed drawing. */
    width?: number;
    /** Fixed drawing height in px. Omitted, one and a half rows of the Density's control height. */
    height?: number;
    /**
         * When LayerChart loads: `lazy` (default) once the figure nears the viewport, `eager` as soon as
         * it mounts. Until then — and in the server render — the figure holds a placeholder of the
         * chart's own Density-sized height under its accessible name.
         */
    loading?: ChartLoading;
}
