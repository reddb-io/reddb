<script lang="ts">
  import type { HTMLAttributes } from "svelte/elements";
  import DescriptionList from "./DescriptionList.svelte";
  import { statistic } from "./statistic.variants";

  import { valuePresentation } from "./core/operational-motion";
  interface Props extends Omit<HTMLAttributes<HTMLDListElement>, "class"> {
    /** Caller-owned name for the measured value. */
    label: string;
    /** Machine-readable scalar; formatting changes only its visible form. */
    value: number | string;
    /**
     * Formats `value` for the visible reading; the machine-readable `data` value is unchanged.
     * Defaults to `String`.
     */
    formatValue?: (value: number | string) => string;
    /** Optional supporting text shown beside the value, such as a period or comparison. */
    description?: string;
    /** Brief paint-only feedback on value changes; semantic readings update immediately. */
    animate?: boolean;
    /** Extra classes, merged over the root slot's own. */
    class?: string;
  }

  let {
    label,
    value,
    formatValue = String,
    description,
    animate = false,
    class: className,
    ...rest
  }: Props = $props();
  const slots = $derived(statistic());
  const formatted = $derived(formatValue(value));
  function feedback(
    node: HTMLDataElement,
    initial: { value: number | string; enabled: boolean },
  ) {
    const p = valuePresentation(node);
    p.update(initial.value, initial.enabled);
    return {
      update(next: { value: number | string; enabled: boolean }) {
        p.update(next.value, next.enabled);
      },
      destroy() {
        p.destroy();
      },
    };
  }
</script>

{#snippet detail()}
  <span class={slots.detail()}>
    <data
      use:feedback={{ value, enabled: animate }}
      data-statistic-value
      value={String(value)}
      class={slots.value()}>{formatted}</data
    >
    {#if description !== undefined}
      <span data-statistic-description class={slots.description()}
        >{description}</span
      >
    {/if}
  </span>
{/snippet}

<DescriptionList
  {...rest as Record<string, unknown>}
  data-statistic
  items={[{ term: label, detail }]}
  class={slots.root({ class: className })}
/>
