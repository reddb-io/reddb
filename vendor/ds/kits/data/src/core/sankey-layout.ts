import { sankey } from "d3-sankey";
import type { SankeyData, SankeyNode, SankeyLink } from "../sankey";
/** Coordinate-bearing nodes after D3 has completed the acyclic flow layout. */
export interface SankeyNodeGeometry extends SankeyNode {
    x0: number;
    x1: number;
    y0: number;
    y1: number;
    value: number;
}
/** Coordinate-bearing links with resolved endpoint objects and flow width. */
export interface SankeyLinkGeometry extends Omit<SankeyLink, "source" | "target"> {
    source: SankeyNodeGeometry;
    target: SankeyNodeGeometry;
    y0: number;
    y1: number;
    width: number;
}
/** Fresh input copies protect caller data from the renderer-neutral engine's mutation. */
export function layoutSankey(data: SankeyData, width: number, height: number, nodeWidth = 12, nodePadding = 24) {
    const result = sankey<SankeyNode, SankeyLink>().size([Math.max(1, width), Math.max(1, height)]).nodeId(node => node.id).nodeWidth(nodeWidth).nodePadding(nodePadding)({ nodes: data.nodes.map(node => ({ ...node })), links: data.links.map(link => ({ ...link })) });
    return result as unknown as {
        nodes: SankeyNodeGeometry[];
        links: SankeyLinkGeometry[];
    };
}
