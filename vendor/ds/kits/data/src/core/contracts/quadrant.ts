import type { VisualizationSize } from "../../visualization";
import type { ScatterPoint } from "../../scatter";
/** Portable Quadrant properties shared by native adapters. */
export interface QuadrantContract {
    /** Named observations placed on the two quantitative axes. */
    data: readonly ScatterPoint[];
    /** Full horizontal domain; defaults to zero through one hundred. */
    xDomain?: readonly [
        number,
        number
    ];
    /** Full vertical domain; defaults to zero through one hundred. */
    yDomain?: readonly [
        number,
        number
    ];
    /** Dividing coordinates; omitted, uses each domain midpoint. */
    thresholds?: readonly [
        number,
        number
    ];
    /** Region names ordered upper-left, upper-right, lower-left, lower-right. */
    regions?: readonly [
        string,
        string,
        string,
        string
    ];
    /** Name of the horizontal measurement. */
    xLabel?: string;
    /** Name of the vertical measurement. */
    yLabel?: string;
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
