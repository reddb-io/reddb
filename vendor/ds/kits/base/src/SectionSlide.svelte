<!-- A divider between parts of a deck: eyebrow, display title and an optional summary (ADR 0027). -->
<script lang="ts">
  import type { HTMLAttributes } from "svelte/elements";
  import Eyebrow from "./Eyebrow.svelte";
  import Heading from "./Heading.svelte";
  import SlideFrame from "./SlideFrame.svelte";
  import type { HeadingLevel } from "./heading.variants";
  import type { SlideSurface } from "./slide-frame.variants";
  import { slideLayout } from "./slide-layout.variants";

  interface Props extends Omit<HTMLAttributes<HTMLDivElement>, "class" | "role" | "title"> {
    /** The section's title, set in the display role. */
    title: string;
    /** A micro-label above the title, such as "Part 2". */
    eyebrow?: string;
    /** One line on what the section covers, in the heading role and secondary ink. */
    summary?: string;
    /** The title's outline level. Defaults to 2. */
    level?: HeadingLevel;
    /** The slide's accessible name. Defaults to `title`. */
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
    title,
    eyebrow,
    summary,
    level = 2,
    label,
    number,
    total,
    surface = "background",
    class: className,
    ...rest
  }: Props = $props();

  const slots = slideLayout();
</script>

<SlideFrame {...rest} label={label ?? title} {number} {total} {surface} class={className} data-section-slide>
  <div class={slots.body()}>
    <div class={slots.header({ class: slots.centred() })}>
      {#if eyebrow}<Eyebrow>{eyebrow}</Eyebrow>{/if}
      <Heading {level} role="display">{title}</Heading>
      {#if summary}<p data-slide-summary class={slots.lede()}>{summary}</p>{/if}
    </div>
  </div>
</SlideFrame>
