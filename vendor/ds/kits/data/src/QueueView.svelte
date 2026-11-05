<script module lang="ts">
  export type { QueueSlot } from "./core/contracts/queueview";
</script>

<script lang="ts">
  import { StatusIndicator, quietControl } from "@reddb-io/design-system/base";
  import { uniqueEntities } from "./operational";
  import { operationalView } from "./operational.variants";
  import type { QueueViewContract } from "./core/contracts/queueview";
  interface Props extends QueueViewContract {
    /** Additional root classes. */
    class?: string;
  }
  let {
    label,
    slots,
    selected = null,
    interactive = false,
    onselectedchange,
    emptyLabel = "Empty slot",
    class: className,
  }: Props = $props();
  const visual = operationalView();
</script>

<section
  data-queue-view
  aria-label={label}
  class={visual.root({ class: className })}
>
  <p class={visual.title()}>{label}</p>
  <ol class={visual.queue()}>
    {#each uniqueEntities(slots, "QueueView") as slot (slot.id)}<li
        class={visual.queueSlot()}
        data-slot-id={slot.id}
      >
        {#if interactive && slot.item}<button
            type="button"
            class={quietControl({
              class: [
                visual.cell(),
                "w-full",
                selected === slot.item.id ? visual.selected() : "",
              ].join(" "),
            })}
            aria-pressed={selected === slot.item.id}
            disabled={slot.item.disabled}
            onclick={() => onselectedchange?.(slot.item!.id)}
            ><span class={visual.text()}>{slot.label}</span><span
              class={visual.title()}>{slot.item.label}</span
            ><StatusIndicator
              label={slot.item.state}
              tone={slot.item.tone}
            /></button
          >{:else}<div class={visual.cell()}>
            <span class={visual.text()}>{slot.label}</span><span
              class={visual.title()}>{slot.item?.label ?? emptyLabel}</span
            >{#if slot.item}<StatusIndicator
                label={slot.item.state}
                tone={slot.item.tone}
              />{/if}
          </div>{/if}
      </li>{/each}
  </ol>
</section>
