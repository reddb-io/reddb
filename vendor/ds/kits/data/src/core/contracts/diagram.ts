import type { VisualizationSize } from "../../visualization";
import type { DiagramData } from "../../diagram";
/** Portable Diagram properties shared by native adapters. */
export interface DiagramContract {
    /** Declarative nodes and named directed edges for flows and architecture. */
    data: DiagramData;
    /** Primary reading direction of the automatic layered layout. */
    direction?: "right" | "down";
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
