<script lang="ts">
 import Diagram from "../../src/Diagram.svelte";
 import ExplodedLayers from "../../src/ExplodedLayers.svelte";
 import SequenceDiagram from "../../src/SequenceDiagram.svelte";
 import MatrixHeatmap from "../../src/MatrixHeatmap.svelte";
 import Waterfall from "../../src/Waterfall.svelte";
 import Scatter from "../../src/Scatter.svelte";
 import Quadrant from "../../src/Quadrant.svelte";
 import Sankey from "../../src/Sankey.svelte";
 import Radar from "../../src/Radar.svelte";
 import SchemaDiagram from "../../src/SchemaDiagram.svelte";
 import EntityRelationship from "../../src/EntityRelationship.svelte";
 import Gantt from "../../src/Gantt.svelte";
 import TraceWaterfall from "../../src/TraceWaterfall.svelte";
 import BreakdownBar from "../../src/BreakdownBar.svelte";
 import KpiCard from "../../src/KpiCard.svelte";
 import HexViewer from "../../src/HexViewer.svelte";
 import Diff from "../../src/Diff.svelte";
 import Treemap from "../../src/Treemap.svelte";
 let {name,options={}}:{name:string;options?:Record<string,any>}=$props();
 const components:Record<string,any>={Diagram,ExplodedLayers,SequenceDiagram,MatrixHeatmap,Waterfall,Scatter,Quadrant,Sankey,Radar,SchemaDiagram,EntityRelationship,Gantt,TraceWaterfall,BreakdownBar,KpiCard,HexViewer,Diff,Treemap};
 const Component=$derived(components[name]);
 export function update(next:Record<string,any>){options=next;}
</script>
<Component {...options} />
