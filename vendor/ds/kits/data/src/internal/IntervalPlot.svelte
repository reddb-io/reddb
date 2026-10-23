<script lang="ts">
  import { scheduleOverlays, clippedWindow, markerPaint } from "../core/schedule-overlays";
  import type { IntervalPlotContract } from "../core/contracts/intervalplot";
  import { quietControl } from "@reddb-io/design-system/base";
  import { visualizationControls } from "../visualization-controls.variants";
  import Frame from "./VisualizationFrame.svelte";
  import Canvas from "./ChartCanvas.svelte";
  import Mark from "./VisualizationMark.svelte";
  import { intervalRows, intervalDomain, type TimeInterval } from "../intervals";
  import { edge, ink, mutedInk, seriesColor, tint, project, ticks, number, short, type VisualizationSize } from "../visualization";
  const controls = visualizationControls();
  interface Props extends IntervalPlotContract { onselectedchange?: (id:string)=>void; class?: string; oncollapsedchange?:(ids:readonly string[])=>void;
  }
  let {data,kind,label,description,width,height,size,selected=$bindable(""),onselectedchange,interactive=true,detailHref,class:className="",timeDomain,formatTime=number,formatDuration=number,collapsed=$bindable([]),oncollapsedchange, markers = [], windows = [], showDetails = true}:Props=$props();
  let zoom=$state(1),offset=$state(0);
  const overlays = $derived(scheduleOverlays(markers, windows, data.map(item => item.id)));
  const visible=$derived(intervalRows(data,collapsed));
  const full=$derived(intervalDomain(data,timeDomain));
  const view=$derived([full[0]+(full[1]-full[0])*(1-1/zoom)*offset,full[0]+(full[1]-full[0])*(1-1/zoom)*offset+(full[1]-full[0])/zoom] as const);
  const rows=$derived(visible.map(d=>({...d,value:formatDuration(d.end-d.start),description:`${formatTime(d.start)} → ${formatTime(d.end)} · ${d.tone??"neutral"}`})));
  const active=$derived(data.find(d=>d.id===selected));
  const parents=$derived(data.filter(d=>data.some(c=>c.parentId===d.id)));
  function select(id:string){selected=id;onselectedchange?.(id);}
  function toggle(id:string){collapsed=collapsed.includes(id)?collapsed.filter(x=>x!==id):[...collapsed,id];oncollapsedchange?.(collapsed);}
</script>
<Frame {kind} {label} {description} {width} {height} {size} {selected} {detailHref} {rows} onselect={interactive?select:undefined} class={className}>
  {#snippet children({width:w,height:h,unit})}
    {@const left=Math.min(w*.38,Math.max(128,...visible.map(d=>Math.min(28,d.label.length+d.depth*2)*7+24)))}
    {@const rowHeight=Math.max(unit*.8125,Math.min(unit*1.125,(h-42)/Math.max(1,visible.length)))}
    {@const actualHeight=Math.max(h,42+visible.length*rowHeight)}
    <Canvas width={w} height={actualHeight} {label}>
      {#each overlays.windows as window (window.id)}
        {@const range = clippedWindow(window.start, window.end, view)}
        {@const row = window.intervalId === undefined ? -1 : visible.findIndex(item => item.id === window.intervalId)}
        {#if range && (window.intervalId === undefined || row >= 0)}
          {@const wx = project(range[0], view, left, w - 12)}
          {@const ww = project(range[1], view, left, w - 12) - wx}
          <rect data-schedule-window={window.id} x={wx} y={row < 0 ? 28 : 32 + row * rowHeight} width={ww} height={row < 0 ? actualHeight - 28 : rowHeight} fill="var(--reddb-color-muted)" stroke={edge} stroke-dasharray="3 3"><title>{window.label}: {formatTime(window.start)} → {formatTime(window.end)}</title></rect>
        {/if}
      {/each}
      {#each ticks(view) as tick}
        {@const x=project(tick,view,left,w-12)}
        <line x1={x} x2={x} y1={28} y2={actualHeight} stroke={edge}/>
        <text {x} y={16} text-anchor="middle" fill={mutedInk} class="font-mono text-xs">{formatTime(kind==="TraceWaterfall"?tick-full[0]:tick)}</text>
      {/each}
      {#each visible as item,i (item.id)}
        {@const y=36+i*rowHeight} {@const x=project(item.start,view,left,w-12)} {@const end=project(item.end,view,left,w-12)}
        {@const color=item.tone&&item.tone!=="neutral"?`var(--reddb-color-feedback-${item.tone}-foreground)`:seriesColor(item.series)}
        <line x1={0} x2={w} y1={y+rowHeight-4} y2={y+rowHeight-4} stroke={edge}/>
        {#if kind==="Gantt"}
          {#each item.dependencies??[] as dep}
            {@const source=visible.find(d=>d.id===dep)} {@const si=visible.findIndex(d=>d.id===dep)}
            {#if source}
              {@const dx=project(source.end,view,left,w-12)}
              {#if dx>=left&&dx<=w-12&&x>=left&&x<=w-12}<path d={`M${dx},${36+si*rowHeight+8} H${Math.min(w-8,dx+8)} V${y+8} H${x}`} fill="none" stroke={mutedInk} stroke-dasharray="3 3"/>{/if}
            {/if}
          {/each}
        {/if}
        <Mark id={item.id} label={`${item.label}: ${formatDuration(item.end-item.start)} · ${item.tone??"neutral"}`} {selected} onselect={interactive?select:undefined}>
          <text x={item.depth*12+4} y={y+12} fill={selected===item.id?ink:mutedInk} class="text-xs">{short(item.label,Math.max(6,Math.floor((left-item.depth*12-16)/7)))}</text>
          {#if end>=left&&x<=w-12}
            {@const startX=Math.max(left,x)} {@const bw=Math.max(2,Math.min(w-12,end)-startX)}
            <rect x={startX} {y} width={bw} height={18} rx={3} fill={tint(color,selected===item.id?40:18)} stroke={color} stroke-width={selected===item.id?2:1}/>
            {#if item.progress!==undefined}<rect x={startX} y={y+14} width={bw*item.progress} height={4} fill={color}/>{/if}
            {#if bw>76}<text x={startX+6} y={y+12} fill={ink} class="font-mono text-xs">{formatDuration(item.end-item.start)}</text>{/if}
            {#if item.tone==="danger"}<text x={Math.min(w-22,startX+bw-10)} y={y+12} fill={color} class="text-xs font-medium">!</text>{/if}
          {/if}
        </Mark>
      {/each}
      {#each overlays.markers.filter(marker => marker.time >= view[0] && marker.time <= view[1]) as marker (marker.id)}
        {@const mx = project(marker.time, view, left, w - 12)}
        <g class="pointer-events-none" data-schedule-marker={marker.id}>
          <line x1={mx} x2={mx} y1={28} y2={actualHeight} stroke={markerPaint(marker.tone)} stroke-width={2} stroke-dasharray="4 3" />
          <text x={mx} y={30} text-anchor={mx > (left + w) / 2 ? "end" : "start"} fill={markerPaint(marker.tone)} class="text-xs">{marker.label}</text>
          <title>{marker.label}: {formatTime(marker.time)}</title>
        </g>
      {/each}
    </Canvas>
  {/snippet}
</Frame>
{#if overlays.markers.length || overlays.windows.length}
  <ul aria-label={`${label} schedule references`} class="m-0 mt-[var(--reddb-spatial-gap-sm)] flex list-none flex-wrap gap-[var(--reddb-spatial-gap-md)] p-0 text-caption text-ink-muted">
    {#each overlays.markers as marker (marker.id)}<li>{marker.label}: {formatTime(marker.time)}</li>{/each}
    {#each overlays.windows as window (window.id)}<li>{window.label}: {formatTime(window.start)} → {formatTime(window.end)}</li>{/each}
  </ul>
{/if}
{#if interactive}
  <div class={controls.root()}>
    <label class={controls.field()}>Zoom <input aria-label={`${label} zoom`} type="range" class={controls.range()} min="1" max="16" step="1" bind:value={zoom}/><span class="font-mono">{zoom}×</span></label>
    {#if zoom>1}<label class={controls.field()}>Position <input aria-label={`${label} position`} type="range" class={controls.range()} min="0" max="1" step="0.01" bind:value={offset}/></label>{/if}
    {#each parents as parent}<button type="button" aria-expanded={!collapsed.includes(parent.id)} onclick={()=>toggle(parent.id)} class={quietControl({class:controls.action({ class: "cursor-pointer underline focus-visible:outline-2 focus-visible:outline-focus" })})}>{collapsed.includes(parent.id)?"Expand":"Collapse"} {parent.label}</button>{/each}
  </div>
  {#if active && showDetails}
    <section aria-label={`${active.label} details`} class="mt-[var(--reddb-spatial-gap-md)] border-t border-muted pt-[var(--reddb-spatial-inset-md)] text-xs">
      <p class="m-0 font-medium">{active.label} · {formatDuration(active.end-active.start)} · {active.tone??"neutral"}</p>
      {#if active.description}<p class="text-ink-muted">{active.description}</p>{/if}
      {#if active.attributes}<dl class="grid grid-cols-[auto_1fr] gap-x-[var(--reddb-spatial-gap-lg)] gap-y-[var(--reddb-spatial-gap-sm)] font-mono">{#each Object.entries(active.attributes) as [key,value]}<dt class="text-ink-muted">{key}</dt><dd class="m-0 break-all">{value}</dd>{/each}</dl>{/if}
      {#if active.href}<a href={active.href} class={quietControl({ class: "underline focus-visible:outline-2 focus-visible:outline-focus" })}>Open details</a>{/if}
    </section>
  {/if}
{/if}
