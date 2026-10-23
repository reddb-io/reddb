<!--
  A titled slide whose remaining height holds one caller-owned figure — a chart,
  a diagram, an image or a table (ADR 0027). Charts live in the Data Kit, so
  the figure arrives as a snippet and Base imports no chart engine.
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import Eyebrow from "./Eyebrow.svelte";
  import Heading from "./Heading.svelte";
  import SlideFrame from "./SlideFrame.svelte";
  import Text from "./Text.svelte";
  import type { HeadingLevel } from "./heading.variants";
  import type { SlideSurface } from "./slide-frame.variants";
  import { slideLayout } from "./slide-layout.variants";

  interface Props extends Omit<HTMLAttributes<HTMLDivElement>, "class" | "role" | "title"> {
    /** The slide's title, set in the title role: say what the figure shows. */
    title: string;
    /** A micro-label above the title. */
    eyebrow?: string;
    /** The figure itself. It fills the height left under the title, so a chart should size to its parent. */
    figure: Snippet;
    /** A caption under the figure: its source, unit or period. */
    caption?: string;
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
    figure,
    caption,
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

<SlideFrame {...rest} label={label ?? title} {number} {total} {surface} class={className} data-figure-slide>
  <div class={slots.body()}>
    <div class={slots.header()}>
      {#if eyebrow}<Eyebrow>{eyebrow}</Eyebrow>{/if}
      <Heading {level} role="title">{title}</Heading>
    </div>
    <figure data-slide-figure class={slots.figure()}>
      <div data-slide-figure-body class={slots.figureBody()}>{@render figure()}</div>
      {#if caption}
        <figcaption><Text role="caption">{caption}</Text></figcaption>
      {/if}
    </figure>
  </div>
</SlideFrame>
