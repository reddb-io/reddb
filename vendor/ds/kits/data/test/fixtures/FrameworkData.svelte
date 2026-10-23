<script lang="ts">
 import CodeField from "../../src/CodeField.svelte";
 import Table from "../../src/Table.svelte";
 import Graph from "../../src/Graph.svelte";
 import MetricChart from "../../src/MetricChart.svelte";
 import Sparkline from "../../src/Sparkline.svelte";
 import TimeSeries from "../../src/TimeSeries.svelte";
 import Histogram from "../../src/Histogram.svelte";
 import Heatmap from "../../src/Heatmap.svelte";
 import DocumentViewer from "../../src/DocumentViewer.svelte";
 import TreeView from "../../src/TreeView.svelte";
 import JsonTree from "../../src/JsonTree.svelte";
 import XmlTree from "../../src/XmlTree.svelte";
 import VirtualList from "../../src/VirtualList.svelte";
 import PanZoomViewport from "../../src/PanZoomViewport.svelte";
 import TerminalSession from "../../src/TerminalSession.svelte";
 import GeoMap from "../../src/GeoMap.svelte";
 let {name,options={}}:{name:string;options?:Record<string,any>}=$props();
 const components:Record<string,any>={CodeField,Table,Graph,MetricChart,Sparkline,TimeSeries,Histogram,Heatmap,DocumentViewer,TreeView,JsonTree,XmlTree,TerminalSession,GeoMap};
 const Component=$derived(components[name]);
 export function update(next:Record<string,any>){options=next;}
</script>
{#if name === "VirtualList"}
 <VirtualList {...options} label={options.label} items={options.items}>{#snippet children(item,index)}<button type="button">{index}: {String(item)}</button>{/snippet}</VirtualList>
{:else if name === "PanZoomViewport"}
 <PanZoomViewport {...options} label={options.label} contentWidth={options.contentWidth} contentHeight={options.contentHeight}><button type="button">Inspect node</button></PanZoomViewport>
{:else if name === "Table"}
 <Table {...options} caption={options.caption} columns={options.columns} rows={options.rows}>
  {#snippet actions(context)}<button type="button" onclick={()=>options.action?.(context)}>Export {context.rows.length} rows</button><details><summary>More actions</summary><button type="button">Inspect selection</button></details>{/snippet}
 </Table>
{:else}
 <Component {...options} />
{/if}
