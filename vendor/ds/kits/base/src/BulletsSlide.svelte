<!-- A titled slide of points, one idea per line (ADR 0027). -->
<script lang="ts">
  import type { HTMLAttributes } from "svelte/elements";
  import Eyebrow from "./Eyebrow.svelte";
  import Heading from "./Heading.svelte";
  import SlideFrame from "./SlideFrame.svelte";
  import type { HeadingLevel } from "./heading.variants";
  import type { SlideSurface } from "./slide-frame.variants";
  import { slideLayout } from "./slide-layout.variants";

  interface Props extends Omit<HTMLAttributes<HTMLDivElement>, "class" | "role" | "title"> {
    /** The slide's title, set in the title role. */
    title: string;
    /** The points, in order, each set in the heading role. A slide holds about six. */
    items: readonly string[];
    /** A micro-label above the title. */
    eyebrow?: string;
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
    items,
    eyebrow,
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

<SlideFrame {...rest} label={label ?? title} {number} {total} {surface} class={className} data-bullets-slide>
  <div class={slots.body()}>
    <div class={slots.header()}>
      {#if eyebrow}<Eyebrow>{eyebrow}</Eyebrow>{/if}
      <Heading {level} role="title">{title}</Heading>
    </div>
    <ul data-slide-points class={slots.list()}>
      {#each items as item, index (index)}
        <li class={slots.item()}>{item}</li>
      {/each}
    </ul>
  </div>
</SlideFrame>
