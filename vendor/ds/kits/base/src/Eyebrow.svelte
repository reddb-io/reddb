<!-- The uppercase micro-label above a heading, in the Theme's eyebrow role (ADR 0025). -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import { eyebrow, type EyebrowElement } from "./eyebrow.variants";

  interface Props extends Omit<HTMLAttributes<HTMLElement>, "class"> {
    /** The element rendered: `p` (default), `span` or `div` — never a heading. */
    as?: EyebrowElement;
    /** Extra classes merged onto the rendered element. */
    class?: string;
    /** The micro-label text, usually a short phrase naming the section that follows. */
    children: Snippet;
  }

  const { as = "p", class: className, children, ...rest }: Props = $props();
</script>

<svelte:element this={as} {...rest} data-eyebrow data-type-role="eyebrow" class={eyebrow({ class: className })}
  >{@render children()}</svelte:element
>
