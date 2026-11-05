<!-- The opening slide of a deck: eyebrow, display title, subtitle and presenter line (ADR 0027). -->
<script lang="ts">
  import type { HTMLAttributes } from "svelte/elements";
  import Eyebrow from "./Eyebrow.svelte";
  import Heading from "./Heading.svelte";
  import SlideFrame from "./SlideFrame.svelte";
  import Text from "./Text.svelte";
  import type { HeadingLevel } from "./heading.variants";
  import type { SlideSurface } from "./slide-frame.variants";
  import { slideLayout } from "./slide-layout.variants";

  interface Props extends Omit<HTMLAttributes<HTMLDivElement>, "class" | "role" | "title"> {
    /** The deck's title, set in the display role. */
    title: string;
    /** A micro-label above the title, such as the event or the series. */
    eyebrow?: string;
    /** One line under the title, in the heading role and secondary ink. */
    subtitle?: string;
    /** Who presents and when, at the foot of the slide. */
    presenter?: string;
    /** The title's outline level. Defaults to 2, so a page heading can sit above the deck. */
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
    subtitle,
    presenter,
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

<SlideFrame {...rest} label={label ?? title} {number} {total} {surface} class={className} data-title-slide>
  <div class={slots.body()}>
    <div class={slots.header({ class: slots.centred() })}>
      {#if eyebrow}<Eyebrow>{eyebrow}</Eyebrow>{/if}
      <Heading {level} role="display">{title}</Heading>
      {#if subtitle}<p data-slide-subtitle class={slots.lede()}>{subtitle}</p>{/if}
    </div>
    {#if presenter}
      <Text data-slide-presenter ink="muted" class={slots.foot()}>{presenter}</Text>
    {/if}
  </div>
</SlideFrame>
