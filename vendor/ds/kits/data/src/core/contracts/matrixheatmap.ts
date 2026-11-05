import type { VisualizationSize } from "../../visualization";
import type { MatrixAxis, MatrixCell } from "../../matrix-heatmap";
/** Portable MatrixHeatmap properties shared by native adapters. */
export interface MatrixHeatmapContract {
    /** Row identities in display order. */
    rowAxis: readonly MatrixAxis[];
    /** Column identities in display order. */
    columnAxis: readonly MatrixAxis[];
    /** Sparse cells; null and missing coordinates are unavailable, not zero. */
    data: readonly MatrixCell[];
    /** Fixed intensity domain for comparisons; omitted, follows observed values. */
    valueDomain?: readonly [
        number,
        number
    ];
    /** Formats cell values, accessible titles and the data table. */
    format?: (value: number) => string;
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
