<!-- A section title whose outline depth and visual size are separate decisions. -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import {
    sectionHeading,
    type SectionHeadingLevel,
    type SectionHeadingSize,
  } from "./section-heading.variants";

  interface Props extends Omit<HTMLAttributes<HTMLElement>, "class" | "title"> {
    /** The heading text, rendered in the `h1` to `h6` element that `level` selects. */
    title: string;
    /** Optional supporting paragraph shown under the title; omitted when empty. */
    description?: string;
    /** Optional id for the rendered heading, for an enclosing landmark's aria-labelledby. */
    titleId?: string;
    /**
     * Outline depth from `1` to `6`, choosing the heading element independently of `size`.
     * Defaults to `2`.
     */
    level?: SectionHeadingLevel;
    /**
     * Visual size of the title: `sm`, `md`, `lg` or the Theme's `display` role, independent of
     * `level`. Defaults to `md`.
     */
    size?: SectionHeadingSize;
    /** Whether a divider is drawn under the heading and its spacing kept. Defaults to `true`. */
    rule?: boolean;
    /** Trailing controls, such as Buttons, placed at the inline end of the heading. */
    actions?: Snippet;
    /** Extra classes, merged over the root slot's own. */
    class?: string;
  }

  const {
    title,
    description,
    titleId,
    level = 2,
    size = "md",
    rule = true,
    actions,
    class: className,
    ...rest
  }: Props = $props();

  const slots = $derived(sectionHeading({ size, rule }));
</script>

<div {...rest} data-section-heading class={slots.root({ class: className })}>
  <div class={slots.text()}>
    <svelte:element this={`h${level}`} id={titleId} data-section-heading-title class={slots.title()}>{title}</svelte:element>
    {#if description}<p data-section-heading-description class={slots.description()}>{description}</p>{/if}
  </div>
  {#if actions}
    <div class={slots.actions()}>{@render actions()}</div>
  {/if}
</div>
