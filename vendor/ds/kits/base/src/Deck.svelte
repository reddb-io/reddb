<!--
  A named run of slides, stacked in reading order (ADR 0027). It is not a deck
  engine: keyboard navigation and speaker notes are follow-up work.
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import { deck } from "./deck.variants";

  interface Props extends Omit<HTMLAttributes<HTMLElement>, "class"> {
    /** The deck's accessible name; it makes the deck a named region of the page. */
    label: string;
    /** Extra classes merged onto the deck. */
    class?: string;
    /** The slides, each a SlideFrame or a slide layout. */
    children?: Snippet;
  }

  const { label, class: className, children, ...rest }: Props = $props();
</script>

<section {...(rest as Record<string, unknown>)} aria-label={label} data-deck class={deck({ class: className })}>
  {@render children?.()}
</section>
