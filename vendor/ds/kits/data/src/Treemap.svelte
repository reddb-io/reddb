<script module lang="ts">
  export type { VisualizationSize, VisualizationSeries } from "./visualization";
  export type { TreemapNode } from "./treemap";
</script>
<script lang="ts">
  import type { TreemapContract } from "./core/contracts/treemap";
  import Frame from "./internal/VisualizationFrame.svelte";
  import Mark from "./internal/VisualizationMark.svelte";
  import { edge, ink, mutedInk, surface, seriesColor, tint, number, short, type VisualizationSize } from "./visualization";
  import Canvas from "./internal/ChartCanvas.svelte";
  import { Treemap as TreemapLayout } from "layerchart/svg";
  import { hierarchy } from "d3-hierarchy";
  import { treemapNodes, type TreemapNode } from "./treemap";
  interface Props extends TreemapContract {
    /** Reports selection for application-owned filtering or detail navigation. */
    onselectedchange?: (id: string) => void;
    /** Extra classes on the outer figure. */
    class?: string;
  }
  let { data, format = number, rootId, label = "Treemap", description, width, height, size = "lg", selected = $bindable(""), onselectedchange, interactive = true, detailHref, class: className = "" }: Props = $props();
  function select(id: string) { selected = id; onselectedchange?.(id); }
  const all=$derived(treemapNodes(data));
  const root=$derived(all.find(n=>n.id===rootId)??data);
  const tree=$derived(hierarchy<TreemapNode>(root,d=>d.children).sum(d=>d.children?.length?0:d.value??0).sort((a,b)=>(b.value??0)-(a.value??0)||a.data.id.localeCompare(b.data.id)));
  const rows=$derived(tree.descendants().map(n=>({...n.data,value:format(n.value??0)})));
</script>
<Frame kind="Treemap" {label} {description} {width} {height} {size} {selected} {detailHref} rows={rows} onselect={interactive ? select : undefined} class={className} >
  {#snippet children({ width: w, height: h })}
    <Canvas width={w} height={h} {label}>
      {#if (tree.value??0)>0}
        <TreemapLayout hierarchy={tree} paddingInner={4} paddingTop={(node)=>node.depth===0?0:node.children?28:4}>
          {#snippet children({nodes})}
            {#each nodes.filter(n=>n.depth>0||!n.children) as node (node.data.id)}
              {@const nw=node.x1-node.x0} {@const nh=node.y1-node.y0} {@const color=seriesColor(node.data.series)}
              <Mark id={node.data.id} label={`${node.data.label}: ${format(node.value??0)}`} {selected} onselect={interactive?select:undefined}>
                <rect x={node.x0} y={node.y0} width={nw} height={nh} fill={node.children?"none":tint(color,selected===node.data.id?36:14)} stroke={selected===node.data.id?ink:edge} stroke-width={selected===node.data.id?2:1}/>
                {#if nw>65&&nh>25}<text x={node.x0+8} y={node.y0+18} fill={ink} class="text-xs">{short(node.data.label,Math.floor((nw-16)/7))}</text>{/if}
                {#if !node.children&&nw>80&&nh>55}<text x={node.x0+8} y={node.y0+38} fill={mutedInk} class="font-mono text-xs">{format(node.value??0)}</text>{/if}
              </Mark>
            {/each}
          {/snippet}
        </TreemapLayout>
      {/if}
    </Canvas>
  {/snippet}
</Frame>
