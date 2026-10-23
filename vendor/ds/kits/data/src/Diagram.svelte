<script module lang="ts">
  export type { VisualizationSize, VisualizationSeries } from "./visualization";
  export type { DiagramData, DiagramNode, DiagramEdge } from "./diagram";
</script>
<script lang="ts">
  import type { DiagramContract } from "./core/contracts/diagram";
  import Frame from "./internal/VisualizationFrame.svelte";
  import Mark from "./internal/VisualizationMark.svelte";
  import { ink, mutedInk, surface, seriesColor, tint, short, type VisualizationSize } from "./visualization";
  import { diagramLayout, diagramDimensions, diagramConnection, type DiagramData } from "./diagram";
  interface Props extends DiagramContract {
    /** Reports selection for application-owned filtering or detail navigation. */
    onselectedchange?: (id: string) => void;
    /** Extra classes on the outer figure. */
    class?: string;
  }
  let { data, direction = "right", label = "Diagram", description, width, height, size = "md", selected = $bindable(""), onselectedchange, interactive = true, detailHref, class: className = "" }: Props = $props();
  function select(id: string) { selected = id; onselectedchange?.(id); }
  const scale = $derived(({ sm: .875, md: 1, lg: 1.125 })[size]);
  const dimensions = $derived(diagramDimensions(data, direction));
  const rows=$derived([...data.nodes.map(n=>({...n,value:n.description??n.kind??"process"})),...data.edges.map(e=>({...e,value:`${e.source} → ${e.target}`}))]);
</script>
<Frame kind="Diagram" {label} {description} {width} {height} {size} widthUnits={dimensions.widthUnits * scale} heightUnits={dimensions.heightUnits * scale} {selected} {detailHref} rows={rows} onselect={interactive ? select : undefined} class={className} >
  {#snippet children({ width: w, height: h, unit })}
    {@const u = unit * scale}
    {@const scene = diagramLayout(data, w, h, direction, u)}
    <svg xmlns="http://www.w3.org/2000/svg" width={w} height={h} viewBox={`0 0 ${w} ${h}`} role="group" aria-label={label}>
      <title>{label}</title>
      {#each scene.edges as e (e.id)}
        {@const a = scene.nodes.find(n => n.id === e.source)!}
        {@const b = scene.nodes.find(n => n.id === e.target)!}
        {@const connection = diagramConnection(a, b, direction, u)}
        {@const active = selected === e.id}
        <Mark id={e.id} label={e.label} {selected} onselect={interactive ? select : undefined}>
          <path d={connection.path} fill="none" stroke={active ? ink : mutedInk} stroke-width={active ? 2 : 1.25} stroke-dasharray={e.dashed ? "5 5" : undefined}/>
          <path d={connection.arrow} fill="none" stroke={active ? ink : mutedInk} stroke-width={active ? 2 : 1.25} stroke-linejoin="round"/>
          <text x={connection.labelX} y={connection.labelY} text-anchor={connection.labelAnchor} fill={mutedInk} class="text-xs">{short(e.label, 20)}</text>
        </Mark>
      {/each}
      {#each scene.nodes as n (n.id)}
        {@const left = n.x - n.width / 2} {@const top = n.y - n.height / 2}
        {@const active = selected === n.id}
        {@const fill = n.series ? tint(seriesColor(n.series), active ? 16 : 8) : active ? tint(ink, 8) : surface}
        {@const stroke = active ? ink : n.series ? tint(seriesColor(n.series), 55) : tint(ink, 36)}
        <Mark id={n.id} label={`${n.label}${n.description ? ": " + n.description : ""}`} {selected} onselect={interactive ? select : undefined}>
          {#if n.kind === "decision"}
            <path d={`M${n.x},${top} L${left+n.width},${n.y} L${n.x},${top+n.height} L${left},${n.y} Z`} {fill} {stroke} stroke-width={active ? 2 : 1.25} stroke-linejoin="round"/>
          {:else if n.kind === "store"}
            <path d={`M${left},${top+u*.3} Q${left},${top} ${n.x},${top} Q${left+n.width},${top} ${left+n.width},${top+u*.3} V${top+n.height-u*.3} Q${left+n.width},${top+n.height} ${n.x},${top+n.height} Q${left},${top+n.height} ${left},${top+n.height-u*.3} Z`} {fill} {stroke} stroke-width={active ? 2 : 1.25}/>
            <path d={`M${left},${top+u*.3} Q${left},${top+u*.6} ${n.x},${top+u*.6} Q${left+n.width},${top+u*.6} ${left+n.width},${top+u*.3}`} fill="none" {stroke}/>
          {:else}
            <rect x={left} y={top} width={n.width} height={n.height} rx={n.kind === "terminal" ? n.height / 2 : u / 4} {fill} {stroke} stroke-width={active ? 2 : 1.25}/>
          {/if}
          <text x={n.x} y={n.y + (n.description ? -3 : 5)} text-anchor="middle" fill={ink} class="text-sm font-medium">{short(n.label, Math.floor((n.width-u) / 7.5))}</text>
          {#if n.description}<text x={n.x} y={n.y+18} text-anchor="middle" fill={mutedInk} class="text-xs">{short(n.description, Math.floor((n.width-u) / 6.5))}</text>{/if}
        </Mark>
      {/each}
    </svg>
  {/snippet}
</Frame>
