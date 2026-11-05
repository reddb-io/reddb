import type { SchemaData } from "../../schema-diagram";
import type { VisualizationSize } from "../../visualization";
/** Portable SchemaPlot properties shared by native adapters. */
export interface SchemaPlotContract {
    data: SchemaData;
    kind: "SchemaDiagram" | "EntityRelationship";
    label: string;
    description?: string;
    width?: number;
    height?: number;
    size?: VisualizationSize;
    selected?: string;
    interactive?: boolean;
    detailHref?: string;
    fieldLimit?: number;
}
