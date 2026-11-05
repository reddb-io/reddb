import type { TimeInterval } from "../../intervals";
import type { VisualizationSize } from "../../visualization";
/** Portable TraceWaterfall properties shared by native adapters. */
export interface TraceWaterfallContract {
    /** Intervals with start/end in one caller-owned time unit, optional parent ids, status and attributes. */
    data: readonly TimeInterval[];
    /** Exact visible time range; omitted, includes the complete data. */
    timeDomain?: readonly [
        number,
        number
    ];
    /** Formats axis timestamps; trace axes receive offsets relative to the request start. */
    formatTime?: (value: number) => string;
    /** Formats durations in bars, tables and detail panels. */
    formatDuration?: (value: number) => string;
    /** Collapsed parent ids; child spans remain in the data and reappear on expansion. */
    collapsed?: readonly string[];
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
