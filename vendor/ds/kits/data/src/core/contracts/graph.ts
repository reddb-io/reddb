import type { GraphData, GraphLayout } from "../../graph";
/** Portable Graph properties shared by native adapters. */
export interface GraphContract {
    /**
         * The network to draw: `nodes` (a stable `id` each, plus optional label, community, token
         * colour, size, position and caller-owned `metadata`) and `edges` between node ids.
         */
    data: GraphData;
    /** Accessible name for the complete network. */
    label?: string;
    /**
         * Logical width of the drawing area in px, used for layout and the SVG `viewBox`; the graph
         * still stretches to its container's width. Defaults to `640`.
         */
    width?: number;
    /** Height of the drawing area in px. Defaults to `400`. */
    height?: number;
    /** Force the dependency-free fallback for SSR, print, tests, or constrained clients. */
    renderer?: "auto" | "svg";
    /**
         * Shows a search field above the graph that finds nodes by label and focuses the chosen match.
         * Defaults to `true`.
         */
    searchable?: boolean;
    /**
         * Id of the focused node, or `null` for none. Passing it (even `null`) makes focus
         * caller-controlled; omitted, the Graph tracks focus itself.
         */
    focusedNode?: string | null;
    /** Communities initially represented by one member-sized supernode. */
    collapsedCommunities?: readonly string[];
    /** Sparse-stress arrangement; force and radial remain as legacy aliases. */
    layout?: GraphLayout;
    /** Deterministic seed shared by the SVG fallback and lazy engine. */
    layoutSeed?: number;
    /** Optional number of top-backbone-betweenness roots to separate. */
    hubCount?: number;
    /** Surface metadata for the last interaction below the graph. */
    showDetails?: boolean;
}
