import type { VisualizationSize } from "../../visualization";
import type { RadarAxis, RadarSeries } from "../../radar";
/** Portable Radar properties shared by native adapters. */
export interface RadarContract {
    /** At least three axes, each with explicit optional min/max and measurement direction. */
    axes: readonly RadarAxis[];
    /** Named series keyed by axis id; null or absent values leave an open path. */
    data: readonly RadarSeries[];
    /** Visible title and accessible name of the visualization. */
    label?: string;
    /** Supporting context, units, aggregation or sampling information. */
    description?: string;
    /** Explicit drawing width in pixels; omitted, follows the container with contained horizontal overflow on small screens. */
    width?: number;
    /** Explicit drawing height in pixels; omitted, follows Density and size. */
    height?: number;
    /** Density-relative drawing height, shared across the visualization family. */
    size?: VisualizationSize;
    /** Selected item id, bindable and retained across data refreshes. */
    selected?: string;
    /** Enables pointer and keyboard activation; false keeps a static readable figure. */
    interactive?: boolean;
    /** Optional native link to the application's detailed view. */
    detailHref?: string;
}
