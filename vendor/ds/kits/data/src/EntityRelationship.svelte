<script module lang="ts">
  export type { SchemaData, SchemaEntity, SchemaField, SchemaRelation } from "./schema-diagram";
  export type { VisualizationSize } from "./visualization";
</script>
<script lang="ts">
  import type { EntityRelationshipContract } from "./core/contracts/entityrelationship";
  import Plot from "./internal/SchemaPlot.svelte";
  import type { SchemaData } from "./schema-diagram";
  import type { VisualizationSize } from "./visualization";
  interface Props extends EntityRelationshipContract {
    /** Reports selection for application-owned filtering or detail navigation. */
    onselectedchange?: (id: string) => void;
    /** Extra classes on the outer figure. */
    class?: string;
  }
  let { data, fieldLimit=8, label="Entity relationships",description,width,height,size,selected=$bindable(""),onselectedchange,interactive=true,detailHref,class:className="" }:Props=$props();
</script>
<Plot kind="EntityRelationship" {data} {fieldLimit} {label} {description} {width} {height} {size} bind:selected {onselectedchange} {interactive} {detailHref} class={className}/>
