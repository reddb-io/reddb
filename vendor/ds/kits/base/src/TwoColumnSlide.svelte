<!-- A titled slide with two equal columns of caller-owned content (ADR 0027). -->
<script lang="ts">
  import type { Snippet } from "svelte";
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
    /** A micro-label above the title. */
    eyebrow?: string;
    /** The first column, in reading order (left in a left-to-right script). */
    start: Snippet;
    /** The second column, in reading order (right in a left-to-right script). */
    end: Snippet;
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
    start,
    end,
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

<SlideFrame {...rest} label={label ?? title} {number} {total} {surface} class={className} data-two-column-slide>
  <div class={slots.body()}>
    <div class={slots.header()}>
      {#if eyebrow}<Eyebrow>{eyebrow}</Eyebrow>{/if}
      <Heading {level} role="title">{title}</Heading>
    </div>
    <div data-slide-columns class={slots.columns()}>
      <div data-slide-column="start" class={slots.column()}>{@render start()}</div>
      <div data-slide-column="end" class={slots.column()}>{@render end()}</div>
    </div>
  </div>
</SlideFrame>
