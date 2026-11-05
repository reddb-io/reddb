<script module lang="ts">
  export type { InspectorEntity } from "../core/entity-inspector";
</script>
<script lang="ts">
  import type { Snippet } from "svelte";
  import { Button } from "@reddb-io/design-system/base";
  import type { EntityInspectorContract } from "../core/contracts/entityinspector";
  import { inspectedEntity } from "../core/entity-inspector";
  import { entityInspector } from "../entity-inspector.variants";
  interface Props extends EntityInspectorContract {
    /** Caller-owned details for the selected entity. */
    children?: Snippet;
    /** Caller-owned actions below the selected entity details. */
    actions?: Snippet;
    /** Additional root classes, including application-owned height constraints. */
    class?: string;
  }
  let { label, items, selected, context, onselectedchange, ondismiss, previousLabel = "Previous", nextLabel = "Next", closeLabel = "Close", emptyLabel = "Select an item to inspect", headingLevel = 2, children, actions, class: className }: Props = $props();
  const s = entityInspector();
  const current = $derived(inspectedEntity(items, selected));
</script>
<section data-entity-inspector data-entity-id={current.entity?.id} aria-label={label} class={s.root({ class: className })}>
  <div class={s.toolbar()}>
    <div class={s.navigation()}>
      <Button variant="ghost" size="sm" disabled={!onselectedchange || !current.previous} onclick={() => current.previous && onselectedchange?.(current.previous)}>{previousLabel}</Button>
      <span class={s.position()}>{current.index < 0 ? 0 : current.index + 1} / {items.length}</span>
      <Button variant="ghost" size="sm" disabled={!onselectedchange || !current.next} onclick={() => current.next && onselectedchange?.(current.next)}>{nextLabel}</Button>
    </div>
    {#if ondismiss}<Button variant="ghost" size="sm" onclick={ondismiss}>{closeLabel}</Button>{/if}
  </div>
  <div class={s.body()}>
    {#if current.entity}
      {#if context}<p class={s.context()}>{context}</p>{/if}
      <svelte:element this={`h${headingLevel}`} class={s.title()}>{current.entity.label}</svelte:element>
      {#if current.entity.description}<p class={s.context()}>{current.entity.description}</p>{/if}
      {@render children?.()}
      {#if actions}<div class={s.actions()}>{@render actions()}</div>{/if}
    {:else}<p class={s.context()}>{emptyLabel}</p>{/if}
  </div>
</section>
