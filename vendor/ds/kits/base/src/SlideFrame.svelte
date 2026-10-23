<!--
  A fixed 16:9 slide (ADR 0027): a size container with a safe title area whose
  content scales with the frame's own width (see slide-frame.variants.ts).
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import { slideFrame, type SlideSurface } from "./slide-frame.variants";

  interface Props extends Omit<HTMLAttributes<HTMLDivElement>, "class" | "role"> {
    /** The slide's accessible name, announced with the "slide" role description. */
    label: string;
    /** The slide's position in its deck, shown in the bottom margin when given. */
    number?: number;
    /** How many slides the deck has; shown after `number` when both are given. */
    total?: number;
    /** The ground the slide is set on: the page `background` (default) or the `raised` elevation. */
    surface?: SlideSurface;
    /** Extra classes merged onto the frame. */
    class?: string;
    /** The slide's content, laid out inside the safe title area of the canvas. */
    children?: Snippet;
  }

  const {
    label,
    number,
    total,
    surface = "background",
    class: className,
    children,
    ...rest
  }: Props = $props();

  const slots = $derived(slideFrame({ surface }));
  // Read aloud in words; shown as the compact "3 / 8" a slide footer carries.
  const position = $derived(total === undefined ? `Slide ${number}` : `Slide ${number} of ${total}`);
  const glyphs = $derived(total === undefined ? `${number}` : `${number} / ${total}`);
</script>

<div
  {...(rest as Record<string, unknown>)}
  role="group"
  aria-roledescription="slide"
  aria-label={label}
  data-slide-frame
  data-slide-surface={surface}
  data-slide-number={number}
  data-slide-total={total}
  class={slots.root({ class: className })}
>
  <div data-slide-stage class={slots.stage()}>
    <div data-slide-safe-area class={slots.safeArea()}>
      {@render children?.()}
    </div>
    {#if number !== undefined}
      <p data-slide-number-label class={slots.number()}>
        <span class="sr-only">{position}</span>
        <span aria-hidden="true">{glyphs}</span>
      </p>
    {/if}
  </div>
</div>
