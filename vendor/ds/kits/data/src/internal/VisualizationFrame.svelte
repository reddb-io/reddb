<script lang="ts">
  import { quietControl } from "@reddb-io/design-system/base";
  import { visualizationControls } from "../visualization-controls.variants";
  import type { Snippet } from "svelte";
  import { seriesColor, type VisualizationSeries, type VisualizationRow, type VisualizationSize } from "../visualization";
  const controls = visualizationControls();
  interface Props {
    label: string;
    description?: string;
    kind: string;
    width?: number;
    height?: number;
    widthUnits?: number;
    heightUnits?: number;
    size?: VisualizationSize;
    rows?: readonly VisualizationRow[];
    legend?: readonly {id:string;label:string;series?:VisualizationSeries}[];
    selected?: string;
    onselect?: (id: string) => void;
    detailHref?: string;
    class?: string;
    children: Snippet<[{ width: number; height: number; unit: number }]>;
  }
  let { label, description, kind, width, height, widthUnits, heightUnits, size = "lg", rows = [], legend = [], selected, onselect, detailHref, class: className = "", children }: Props = $props();
  const count = { sm: 6, md: 10, lg: 14 };
  let spatialUnit = $state(0);
  let measuredWidth = $state(0);
  let measuredHeight = $state(0);
  const w = $derived(width ?? Math.max(widthUnits === undefined ? 480 : widthUnits * (spatialUnit || 32), measuredWidth || 800));
  const h = $derived(height ?? (measuredHeight || (heightUnits ?? count[size]) * 32));
</script>

<figure data-visualization={kind} class={`relative m-0 flex w-full min-w-0 flex-col gap-[var(--reddb-spatial-gap-md)] text-foreground ${className}`}>
  <figcaption class="flex flex-wrap items-start justify-between gap-[var(--reddb-spatial-gap-sm)]">
    <div class="min-w-0">
      <p class="m-0 text-sm font-medium">{label}</p>
      {#if description}<p class="m-0 mt-[var(--reddb-spatial-gap-sm)] max-w-prose text-xs text-ink-muted">{description}</p>{/if}
    </div>
    {#if detailHref}<a href={detailHref} class={quietControl({ class: "text-xs text-foreground underline underline-offset-4 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus" })}>Explore</a>{/if}
  </figcaption>
  <span aria-hidden="true" bind:clientHeight={spatialUnit} class="pointer-events-none invisible absolute h-[var(--reddb-spatial-control-height-md)] w-[var(--reddb-spatial-inset-sm)]"></span>

<!-- svelte-ignore a11y_no_noninteractive_tabindex (Keyboard access to the scroll region or conditionally interactive mark.) -->
  <div class="min-w-0 overflow-x-auto" tabindex="0" role="region" aria-label={`${label} visualization`}>
    <div bind:clientWidth={measuredWidth} bind:clientHeight={measuredHeight}
      class="min-w-[calc(var(--reddb-spatial-control-height-md)*15)]"
      style:min-width={widthUnits === undefined ? undefined : `calc(var(--reddb-spatial-control-height-md) * ${widthUnits})`}
      style:width={width === undefined ? undefined : `${width}px`}
      style:height={height === undefined ? `calc(var(--reddb-spatial-control-height-md) * ${heightUnits ?? count[size]})` : `${height}px`}>
      {@render children({ width: w, height: h, unit: spatialUnit || 32 })}
    </div>
  </div>
  {#if legend.length}
    <ul aria-label={`${label} legend`} class="m-0 flex list-none flex-wrap gap-[var(--reddb-spatial-gap-md)] p-0 text-xs">
      {#each legend as item (item.id)}
        <li class="flex items-center gap-[var(--reddb-spatial-gap-sm)]">
          <span aria-hidden="true" class="inline-block h-[var(--reddb-spatial-icon-size-sm)] w-[var(--reddb-spatial-icon-size-sm)] rounded-full" style:background={seriesColor(item.series)}></span>
          {#if onselect}<button type="button" class={quietControl({class:controls.action({ class: "cursor-pointer" })})} aria-pressed={selected===item.id} onclick={()=>onselect?.(item.id)}>{item.label}</button>{:else}{item.label}{/if}
        </li>
      {/each}
    </ul>
  {/if}
  {#if rows.length}
    <details class="text-xs">
      <summary class={controls.summary({ class: "w-fit cursor-pointer text-ink-muted focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus" })}>Explore data ({rows.length})</summary>
      <div class="mt-[var(--reddb-spatial-gap-sm)] overflow-auto" style:max-height="calc(var(--reddb-spatial-control-height-md) * 8)">
        <table class="w-full border-collapse text-start text-xs">
          <caption class="sr-only">{label} data</caption>
          <thead><tr class="border-b border-muted text-ink-muted"><th scope="col" class="p-[var(--reddb-spatial-inset-sm)] text-start font-medium">Item</th><th scope="col" class="p-[var(--reddb-spatial-inset-sm)] text-end font-medium">Value</th></tr></thead>
          <tbody>{#each rows as row (row.id)}
            <tr class="border-b border-muted" data-selected={selected === row.id ? "" : undefined}>
              <th scope="row" class="p-[var(--reddb-spatial-inset-sm)] text-start font-regular">
                {#if row.href}<a href={row.href} class={quietControl({ class: controls.action({ class: "underline focus-visible:outline-2 focus-visible:outline-focus" }) })}>{row.label}</a>
                {:else if onselect}<button type="button" aria-pressed={selected === row.id} onclick={() => onselect?.(row.id)} class={quietControl({class:controls.action({ class: "cursor-pointer text-start underline focus-visible:outline-2 focus-visible:outline-focus" })})}>{row.label}</button>
                {:else}{row.label}{/if}
                {#if row.description}<span class="block text-ink-muted">{row.description}</span>{/if}
              </th>
              <td class="p-[var(--reddb-spatial-inset-sm)] text-end font-mono tabular-nums">{row.value}</td>
            </tr>
          {/each}</tbody>
        </table>
      </div>
    </details>
  {:else}<p class="m-0 text-xs text-ink-muted">No data available.</p>{/if}
</figure>
