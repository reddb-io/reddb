import type { SchemaData } from "../../schema-diagram";
import type { VisualizationSize } from "../../visualization";
/** Portable SchemaDiagram properties shared by native adapters. */
export interface SchemaDiagramContract {
    /** Entities, fields and named relationships; optional explicit positions preserve application-owned layouts. */
    data: SchemaData;
    /** Maximum visible fields per entity; complete fields remain in Explore data. */
    fieldLimit?: number;
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
