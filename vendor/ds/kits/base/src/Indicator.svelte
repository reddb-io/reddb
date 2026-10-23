<!--
  Indicator — a Base Primitive for placing caller-owned marker content around
  caller-owned content. It adds no status meaning and no interaction of its
  own; Badge or StatusIndicator supplies meaning, while the child keeps focus.
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import { indicator, type IndicatorPosition } from "./indicator.variants";

  interface Props extends Omit<HTMLAttributes<HTMLSpanElement>, "class"> {
    /** Content placed around the anchored child. */
    marker: Snippet;
    /** The content whose box establishes the placement boundary. */
    children: Snippet;
    /** Logical marker anchor. Defaults to top-end. */
    position?: IndicatorPosition;
    /** Extra classes, merged over the placement root. */
    class?: string;
  }

  const {
    marker,
    children,
    position = "top-end",
    class: className,
    ...rest
  }: Props = $props();
  const slots = $derived(indicator({ position }));
</script>

<span {...rest} class={slots.root({ class: className })} data-indicator>
  <span class={slots.marker()} data-indicator-marker>{@render marker()}</span>
  {@render children()}
</span>
