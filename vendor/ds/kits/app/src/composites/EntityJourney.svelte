<script module lang="ts">
  export type { JourneyStage, JourneyState } from "../core/entity-journey";
</script>

<script lang="ts">
  import { StatusIndicator, quietControl } from "@reddb-io/design-system/base";
  import type { EntityJourneyContract } from "../core/contracts/entityjourney";
  import { journeyStages } from "../core/entity-journey";
  import { entityJourney } from "../entity-journey.variants";
  interface Props extends EntityJourneyContract {
    /** Additional root composition classes. */
    class?: string;
  }
  let {
    label,
    items,
    selected = null,
    onselectedchange,
    emptyLabel = "No stages observed",
    class: className,
  }: Props = $props();
  const s = entityJourney();
  const stages = $derived(journeyStages(items));
</script>

<div data-entity-journey class={s.root({ class: className })}>
  <ol aria-label={label} class={s.list()}>
    {#each stages as item, index (item.id)}
      <li
        class={s.stage()}
        data-journey-stage={item.id}
        data-state={item.state}
        aria-current={item.state === "current" ? "step" : undefined}
      >
        {#snippet content()}
          <span aria-hidden="true" class={s.marker({ state: item.state })}>
            {index + 1}
          </span>
          <span class={s.body()}
            ><span class={s.label()}>{item.label}</span><StatusIndicator
              label={item.stateLabel}
              tone={item.tone}
            /><span class={s.note()}>{item.description ?? ""}</span><span
              class={s.elapsed()}>{item.elapsed ?? ""}</span
            ></span
          >
        {/snippet}
        {#if onselectedchange}<button
            type="button"
            aria-pressed={selected === item.id}
            class={quietControl({ ink: "foreground", class: s.item() })}
            onclick={() => onselectedchange?.(item.id)}
            >{@render content()}</button
          >
        {:else}<div class={s.item()}>{@render content()}</div>{/if}
      </li>
    {/each}
  </ol>
  {#if !stages.length}<p class={s.note()}>{emptyLabel}</p>{/if}
</div>
