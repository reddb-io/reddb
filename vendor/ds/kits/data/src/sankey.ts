import { finite, unique, type VisualizationItem, type VisualizationSeries } from "./visualization";
export interface SankeyNode extends VisualizationItem {}
export interface SankeyLink extends VisualizationItem { source:string; target:string; value:number }
export interface SankeyData { nodes:readonly SankeyNode[]; links:readonly SankeyLink[] }
/** Validate before handing fresh copies to LayerChart's mutating layout engine. */
export function sankeyData(data:SankeyData) {
  unique(data.nodes,"Sankey nodes");unique(data.links,"Sankey links");
  const byId=new Map(data.nodes.map(n=>[n.id,n]));const outgoing=new Map<string,string[]>();
  for(const link of data.links) {
    if(!byId.has(link.source)||!byId.has(link.target))throw new Error("Sankey references an unknown node");
    if(finite(link.value,link.id)<0)throw new Error("Sankey weights must be non-negative");
    outgoing.set(link.source,[...(outgoing.get(link.source)??[]),link.target]);
  }
  const active=new Set<string>(),done=new Set<string>();
  function visit(id:string) {if(active.has(id))throw new Error("Sankey requires an acyclic flow; represent retries as separate stages");if(done.has(id))return;active.add(id);for(const next of outgoing.get(id)??[])visit(next);active.delete(id);done.add(id);}
  for(const node of data.nodes)visit(node.id);
  const links=data.links.filter(l=>l.value>0).map(l=>({...l}));
  const used=new Set(links.flatMap(l=>[l.source,l.target]));
  return {nodes:data.nodes.map((n,index)=>({...n,series:n.series??((index%6+1) as VisualizationSeries)})).filter(n=>used.has(n.id)),links};
}
