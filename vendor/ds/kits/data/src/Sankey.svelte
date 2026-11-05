<script module lang="ts">
  export type { VisualizationSize, VisualizationSeries } from "./visualization";
  export type { SankeyData, SankeyNode, SankeyLink } from "./sankey";
</script>
<script lang="ts">
  import type { SankeyContract } from "./core/contracts/sankey";
  import Frame from "./internal/VisualizationFrame.svelte";
  import Mark from "./internal/VisualizationMark.svelte";
  import { edge, ink, mutedInk, surface, seriesColor, tint, number, short, type VisualizationSize } from "./visualization";
  import Canvas from "./internal/ChartCanvas.svelte";
  import { layoutSankey } from "./core/sankey-layout";
  import { sankeyData, type SankeyData } from "./sankey";
  interface Props extends SankeyContract {
    /** Reports selection for application-owned filtering or detail navigation. */
    onselectedchange?: (id: string) => void;
    /** Extra classes on the outer figure. */
    class?: string;
  }
  let { data, format = number, label = "Sankey", description, width, height, size = "lg", selected = $bindable(""), onselectedchange, interactive = true, detailHref, class: className = "" }: Props = $props();
  function select(id: string) { selected = id; onselectedchange?.(id); }
  const flow=$derived(sankeyData(data));
  const rows=$derived(data.links.map(l=>({...l,value:format(l.value),description:`${data.nodes.find(n=>n.id===l.source)?.label} → ${data.nodes.find(n=>n.id===l.target)?.label}`})));
</script>
<Frame kind="Sankey" {label} {description} {width} {height} {size} {selected} {detailHref} rows={rows} onselect={interactive ? select : undefined} class={className} >
  {#snippet children({ width: w, height: h })}
    <Canvas width={w} height={h} {label} data={flow} padding={{left:100,right:100,top:24,bottom:24}}>
      {#if flow.links.length}
        {@const scene = layoutSankey(flow,w-200,h-48)}
            {#each scene.links as link}
              {@const x0=link.source.x1} {@const x1=link.target.x0} {@const mid=(x0+x1)/2}
              <Mark id={link.id} label={`${link.label}: ${format(link.value)}`} {selected} onselect={interactive?select:undefined}>
                <path d={`M${x0},${link.y0} C${mid},${link.y0} ${mid},${link.y1} ${x1},${link.y1}`} fill="none" stroke={seriesColor(link.series??link.source.series)} stroke-opacity={selected===link.id?0.8:0.25} stroke-width={Math.max(1,link.width)}/>
              </Mark>
            {/each}
            {#each scene.nodes as node}
              <Mark id={node.id} label={`${node.label}: ${format(node.value)}`} {selected} onselect={interactive?select:undefined}>
                <rect x={node.x0} y={node.y0} width={node.x1-node.x0} height={node.y1-node.y0} rx={2} fill={seriesColor(node.series)}/>
                <text x={node.x0<1?node.x0-10:node.x1+10} y={(node.y0+node.y1)/2-3} text-anchor={node.x0<1?"end":"start"} fill={ink} class="text-xs">{short(node.label,15)}</text>
                <text x={node.x0<1?node.x0-10:node.x1+10} y={(node.y0+node.y1)/2+13} text-anchor={node.x0<1?"end":"start"} fill={mutedInk} class="font-mono text-xs">{format(node.value)}</text>
              </Mark>
            {/each}

      {/if}
    </Canvas>
  {/snippet}
</Frame>
