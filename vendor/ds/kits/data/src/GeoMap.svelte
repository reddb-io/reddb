<script module lang="ts">
  export type { GeoMapPoint, GeoMapRoute, GeoMapGeography, GeoMapGeometry } from "./geo-map";
</script>
<script lang="ts">
  import type { GeoMapContract } from "./core/contracts/geomap";
  import { Button } from "@reddb-io/design-system/base";
  import PanZoomViewport from "./PanZoomViewport.svelte";
  import Mark from "./internal/VisualizationMark.svelte";
  import { geoData, geoLand, geoLabelIds, MAP_WIDTH, MAP_HEIGHT, type GeoMapPoint, type GeoMapRoute, type GeoMapGeography } from "./geo-map";
  import { geoMap } from "./geo-map.variants";
  import { edge, ink, mutedInk, surface, seriesColor, type VisualizationSize } from "./visualization";
  import type { PanZoomView } from "./pan-zoom";
  interface Props extends GeoMapContract {
    /** Reports pointer, keyboard or data-list selection of a point. */
    onselectedchange?: (id: string) => void;
    /** Extra classes on the figure. */
    class?: string;
  }
  let { label, description, points, routes = [], geography, selected = $bindable(""), onselectedchange,
    interactive = true, size = "md", attribution, dataLabel = "Explore locations and routes", class: className }: Props = $props();
  const id = $props.id();
  const styles = geoMap();
  const data = $derived(geoData(points,routes));
  const land = $derived(geoLand(geography));
  let view = $state<PanZoomView>();
  let glyphWidth = $state(0), lineHeight = $state(0);
  const scale = $derived(view?.scale ?? 1);
  const labels = $derived(geoLabelIds(data.points,selected,scale,glyphWidth||8,lineHeight||16));
  function select(id:string) { selected=id; onselectedchange?.(id); }
</script>

<figure data-geo-map class={styles.root({class:className})} aria-labelledby={`${id}-title`}>
  <figcaption>
    <p id={`${id}-title`} class={styles.title()}>{label}</p>
    {#if description}<p class={styles.description()}>{description}</p>{/if}
  </figcaption>
  <span class="pointer-events-none invisible absolute font-mono text-caption" aria-hidden="true" bind:clientWidth={glyphWidth} bind:clientHeight={lineHeight}>M</span>
  <PanZoomViewport label={`${label} map`} contentWidth={MAP_WIDTH} contentHeight={MAP_HEIGHT} aspectRatio={MAP_WIDTH/MAP_HEIGHT} {size} bind:view>
    <svg xmlns="http://www.w3.org/2000/svg" viewBox={`0 0 ${MAP_WIDTH} ${MAP_HEIGHT}`} class={styles.svg()} role="group" aria-label={label}>
      <defs><clipPath id={`${id}-clip`}><rect width={MAP_WIDTH} height={MAP_HEIGHT}/></clipPath></defs>
      <g clip-path={`url(#${id}-clip)`}>
        <rect width={MAP_WIDTH} height={MAP_HEIGHT} fill={surface}/>
        {#each [-60,-30,0,30,60] as latitude}<path d={`M0,${(90-latitude)/180*MAP_HEIGHT} H${MAP_WIDTH}`} stroke={edge} stroke-width="1" fill="none"/>{/each}
        {#each [-120,-60,0,60,120] as longitude}<path d={`M${(longitude+180)/360*MAP_WIDTH},0 V${MAP_HEIGHT}`} stroke={edge} stroke-width="1" fill="none"/>{/each}
        {#each land as path}<path d={path} fill="var(--reddb-color-muted)" stroke={edge} stroke-width="1" fill-rule="evenodd"/>{/each}
        {#each data.routes as route (route.id)}
          <g role="group" aria-label={route.label}><title>{route.label}</title>
            {#each route.paths as path}<path d={path} fill="none" stroke={seriesColor(route.series)} stroke-width={2/scale}/>{/each}
          </g>
        {/each}
        {#each data.points as point (point.id)}
          <Mark id={point.id} label={point.description ? `${point.label}: ${point.description}` : point.label} {selected} onselect={interactive?select:undefined}>
            <circle cx={point.position[0]} cy={point.position[1]} r={22/scale} fill="transparent"/>
            <circle cx={point.position[0]} cy={point.position[1]} r={(selected===point.id?6:4)/scale} fill={seriesColor(point.series)}
              stroke={selected===point.id?ink:surface} stroke-width={2/scale}/>
            {#if labels.has(point.id)}<text x={point.position[0]+(point.position[0]>800?-12:12)/scale} y={point.position[1]-10/scale}
              text-anchor={point.position[0]>800?"end":"start"} fill={selected===point.id?ink:mutedInk}
              style:font-size={`calc(var(--reddb-type-caption-font-size) / ${scale})`} class="font-mono">{point.label}</text>{/if}
          </Mark>
        {/each}
      </g>
    </svg>
  </PanZoomViewport>
  {#if attribution}<p class={styles.description()}>{attribution}</p>{/if}
  <details class={styles.data()}>
    <summary class="w-fit cursor-pointer focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus">{dataLabel} ({points.length})</summary>
    <ul class={styles.list()} aria-label={`${label} locations`}>
      {#each points as point (point.id)}
        <li class={styles.row()}>
          {#if interactive}<Button variant="ghost" size="sm" aria-pressed={selected===point.id} onclick={()=>select(point.id)}>{point.label}</Button>{:else}<span>{point.label}</span>{/if}
          <span>{point.latitude.toFixed(2)}°, {point.longitude.toFixed(2)}°{point.description?` · ${point.description}`:""}</span>
        </li>
      {/each}
    </ul>
    {#if routes.length}<ul class={styles.list()} aria-label={`${label} routes`}>{#each routes as route (route.id)}<li>{route.label}</li>{/each}</ul>{/if}
  </details>
</figure>
