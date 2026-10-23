<!-- A pull quote with its attribution, filling the slide (ADR 0027). -->
<script lang="ts">
  import type { HTMLAttributes } from "svelte/elements";
  import SlideFrame from "./SlideFrame.svelte";
  import Text from "./Text.svelte";
  import type { SlideSurface } from "./slide-frame.variants";
  import { slideLayout } from "./slide-layout.variants";

  interface Props extends Omit<HTMLAttributes<HTMLDivElement>, "class" | "role"> {
    /** The quotation, without quotation marks: the slide sets them. Set in the title role. */
    quote: string;
    /** Who said it. */
    attribution: string;
    /** Where or in what capacity they said it, after the attribution. */
    source?: string;
    /** The slide's accessible name. Defaults to "Quote from" and the attribution. */
    label?: string;
    /** The slide's position in its deck, shown in the bottom margin when given. */
    number?: number;
    /** How many slides the deck has; shown after `number` when both are given. */
    total?: number;
    /** The ground the slide is set on: the page `background` (default) or the `raised` elevation. */
    surface?: SlideSurface;
    /** Extra classes merged onto the frame. */
    class?: string;
  }

  const {
    quote,
    attribution,
    source,
    label,
    number,
    total,
    surface = "background",
    class: className,
    ...rest
  }: Props = $props();

  const slots = slideLayout();
</script>

<SlideFrame
  {...rest}
  label={label ?? `Quote from ${attribution}`}
  {number}
  {total}
  {surface}
  class={className}
  data-quote-slide
>
  <div class={slots.body()}>
    <figure data-slide-quote class={slots.quote()}>
      <blockquote><p class={slots.quoteText()}>&ldquo;{quote}&rdquo;</p></blockquote>
      <figcaption>
        <Text as="span">{attribution}</Text>{#if source}<Text as="span" ink="muted">, {source}</Text>{/if}
      </figcaption>
    </figure>
  </div>
</SlideFrame>
