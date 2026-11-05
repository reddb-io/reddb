import type { ChartNumberFormat } from "../../chart";
import type { ChartSeries, ChartSize } from "../../chart";
import type { ChartLoading } from "../../chart-loading";
/** Portable Histogram properties shared by native adapters. */
export interface HistogramContract {
    /**
         * The raw numbers to distribute across equal-width bins. Non-finite entries are ignored, and an
         * empty set draws no bars.
         */
    values: readonly number[];
    /**
         * How many equal-width bins to split the range of `values` into (rounded down, at least `1`).
         * Defaults to `10`.
         */
    bins?: number;
    /** Accessible description of the represented distribution. */
    label?: string;
    /**
         * Renders a bin edge in the bin labels. Defaults to a locale-free number
         * with just enough decimals to tell neighbouring edges apart.
         */
    format?: ChartNumberFormat;
    /** The Series Role (1–6) the bars are drawn in. Never the Brand red. */
    series?: ChartSeries;
    /**
         * Fixed drawing width in px. Omitted, the histogram fills its figure's
         * content width and follows it on resize (640 until the figure is measured);
         * the container sets that width, and the chart never widens its container.
         */
    width?: number;
    /** Fixed drawing height in px. Omitted, ten rows of the Density's control height. */
    height?: number;
    /** Shared Density-relative plot height: three, six or ten control rows. An explicit `height` wins. Defaults to ten rows. */
    size?: ChartSize;
    /**
         * When LayerChart loads: `lazy` (default) once the figure nears the viewport, `eager` as soon as
         * it mounts. Until then — and in the server render — the figure holds a placeholder of the
         * chart's own Density-sized height under its accessible name.
         */
    loading?: ChartLoading;
}
