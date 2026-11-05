<script module lang="ts">
  export type { TimeInterval } from "./intervals";
  export type { VisualizationSize } from "./visualization";
</script>
<script lang="ts">
  import type { TraceWaterfallContract } from "./core/contracts/tracewaterfall";
  import Plot from "./internal/IntervalPlot.svelte";
  import type { TimeInterval } from "./intervals";
  import type { VisualizationSize } from "./visualization";
  interface Props extends TraceWaterfallContract {
    /** Reports expanded or collapsed hierarchy state. */
    oncollapsedchange?:(ids: readonly string[]) => void;
    /** Reports selection for application-owned filtering or detail navigation. */
    onselectedchange?: (id: string) => void;
    /** Extra classes on the outer figure. */
    class?: string;
  }
  let { data, label="Request trace", description, width, height, size, selected=$bindable(""), onselectedchange, interactive=true, detailHref, class:className="", timeDomain,formatTime,formatDuration,collapsed=$bindable([]),oncollapsedchange }:Props=$props();
</script>
<Plot kind="TraceWaterfall" {data} {label} {description} {width} {height} {size} bind:selected {onselectedchange} {interactive} {detailHref} class={className} {timeDomain} {formatTime} {formatDuration} bind:collapsed {oncollapsedchange}/>
