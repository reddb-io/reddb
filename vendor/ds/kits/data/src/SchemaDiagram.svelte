<script module lang="ts">
  export type { SchemaData, SchemaEntity, SchemaField, SchemaRelation } from "./schema-diagram";
  export type { VisualizationSize } from "./visualization";
</script>
<script lang="ts">
  import type { SchemaDiagramContract } from "./core/contracts/schemadiagram";
  import Plot from "./internal/SchemaPlot.svelte";
  import type { SchemaData } from "./schema-diagram";
  import type { VisualizationSize } from "./visualization";
  interface Props extends SchemaDiagramContract {
    /** Reports selection for application-owned filtering or detail navigation. */
    onselectedchange?: (id: string) => void;
    /** Extra classes on the outer figure. */
    class?: string;
  }
  let { data, fieldLimit=8, label="Database schema",description,width,height,size,selected=$bindable(""),onselectedchange,interactive=true,detailHref,class:className="" }:Props=$props();
</script>
<Plot kind="SchemaDiagram" {data} {fieldLimit} {label} {description} {width} {height} {size} bind:selected {onselectedchange} {interactive} {detailHref} class={className}/>
