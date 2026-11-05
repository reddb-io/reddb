import type { VisualizationSize } from "../../visualization";
import type { ScatterPoint } from "../../scatter";
/** Portable Scatter properties shared by native adapters. */
export interface ScatterContract {
    /** Observations with stable ids, numeric x/y coordinates and optional radius. */
    data: readonly ScatterPoint[];
    /** Explicit x-axis bounds, shared across comparisons. */
    xDomain?: readonly [
        number,
        number
    ];
    /** Explicit y-axis bounds, shared across comparisons. */
    yDomain?: readonly [
        number,
        number
    ];
    /** Label and unit of the horizontal measurement. */
    xLabel?: string;
    /** Label and unit of the vertical measurement. */
    yLabel?: string;
    /** Draws a least-squares trend when at least two distinct x values exist. */
    trend?: boolean;
    /** Formats the x coordinate in axes, titles and the data table. */
    formatX?: (value: number) => string;
    /** Formats the y coordinate in axes, titles and the data table. */
    formatY?: (value: number) => string;
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
