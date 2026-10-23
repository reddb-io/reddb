<!-- Semantic key caps and chords, available to every Kit through Base. -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import { kbd, kbdChord, type KbdSize } from "./kbd.variants";

  interface Props extends Omit<HTMLAttributes<HTMLElement>, "children" | "class"> {
    /** One key, or the ordered parts of a chord. */
    keys?: readonly string[];
    /** Visual separator hidden from assistive technology. */
    separator?: string;
    /** Key cap size: `sm` or `md` (the default). It follows the surrounding type, not Density. */
    size?: KbdSize;
    /** Extra classes merged onto each key cap, not onto a chord's wrapper. */
    class?: string;
    /** A custom single key cap, replacing `keys`. */
    children?: Snippet;
  }

  const {
    keys = [],
    separator = "+",
    size = "md",
    class: className,
    children,
    ...rest
  }: Props = $props();

  const chord = kbdChord();
</script>

{#if children}
  <kbd {...rest} data-kbd class={kbd({ size, class: className })}>{@render children()}</kbd>
{:else}
  <kbd {...rest} data-kbd class={chord.root()}>
    {#each keys as key, index (index)}
      {#if index > 0}<span aria-hidden="true" class={chord.separator()}>{separator}</span>{/if}
      <kbd class={kbd({ size, class: className })}>{key}</kbd>
    {/each}
  </kbd>
{/if}
