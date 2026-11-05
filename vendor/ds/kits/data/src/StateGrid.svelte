<script module lang="ts">
  export type { StateCell } from "./operational";
</script>

<script lang="ts">
  import { StatusIndicator, quietControl } from "@reddb-io/design-system/base";
  import type { StateGridContract } from "./core/contracts/stategrid";
  import { uniqueEntities, stateCellWidth } from "./operational";
  import { operationalView } from "./operational.variants";
  interface Props extends StateGridContract {
    /** Additional root classes. */
    class?: string;
  }
  let {
    label,
    items,
    cellWidth = 12,
    selected = null,
    interactive = false,
    onselectedchange,
    emptyLabel = "No entities",
    class: className,
  }: Props = $props();
  const s = operationalView();
  const cells = $derived(uniqueEntities(items, "StateGrid"));
  const width = $derived(stateCellWidth(cellWidth));
</script>

<div data-state-grid class={s.root({ class: className })}>
  {#if cells.length}<ul
      aria-label={label}
      class={s.grid()}
      style:grid-template-columns={`repeat(auto-fit, minmax(min(100%, ${width}ch), 1fr))`}
    >
      {#each cells as item (item.id)}<li
          class={s.stateItem()}
          data-entity-id={item.id}
          data-state={item.state}
        >
          {#if interactive}<button
              type="button"
              disabled={item.disabled}
              aria-pressed={selected === item.id}
              onclick={() => onselectedchange?.(item.id)}
              class={quietControl({
                class: [
                  s.cell(),
                  "w-full",
                  selected === item.id ? s.selected() : "",
                ].join(" "),
              })}
              ><span class={s.title()}>{item.label}</span><StatusIndicator
                label={item.state}
                tone={item.tone}
              />{#if item.description}<span class={s.text()}
                  >{item.description}</span
                >{/if}</button
            >{:else}<div class={s.cell()}>
              <span class={s.title()}>{item.label}</span><StatusIndicator
                label={item.state}
                tone={item.tone}
              />{#if item.description}<span class={s.text()}
                  >{item.description}</span
                >{/if}
            </div>{/if}
        </li>{/each}
    </ul>{:else}<p aria-label={label} class={s.text()}>{emptyLabel}</p>{/if}
</div>
