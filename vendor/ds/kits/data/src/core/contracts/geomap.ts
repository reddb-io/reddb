import type { GeoMapPoint, GeoMapRoute, GeoMapGeography } from "../../geo-map";
import type { VisualizationSize } from "../../visualization";
/** Portable GeoMap properties shared by native adapters. */
export interface GeoMapContract {
    /** Visible title and accessible name of the map. */
    label: string;
    /** Supporting context, provenance or geographic limits. */
    description?: string;
    /** Application-owned WGS84 point locations. */
    points: readonly GeoMapPoint[];
    /** Labelled links between point ids. Routes take the shortest longitude crossing. */
    routes?: readonly GeoMapRoute[];
    /** Optional caller-owned GeoJSON polygons. No geography dataset or tile service is bundled. */
    geography?: GeoMapGeography;
    /** Selected point id; opening detail remains application-owned. */
    selected?: string;
    /** Enables point selection. False preserves a readable static map and data list. */
    interactive?: boolean;
    /** Density-relative viewport height. */
    size?: VisualizationSize;
    /** Attribution required by the application's geographic dataset. */
    attribution?: string;
    /** Label of the expandable textual locations and routes. */
    dataLabel?: string;
}
