<!--
  Token-backed flow that inherits the nearest Density scope. The Stack owns the
  rhythm between its own children (ADR 0024); its direction, alignment and
  wrapping are props, so a caller never overrides the flow with classes.
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import {
    stack,
    type StackAlign,
    type StackDirection,
    type StackGap,
    type StackJustify,
  } from "./stack.variants";

  interface Props extends Omit<HTMLAttributes<HTMLDivElement>, "class"> {
    /**
     * Space between children. Each value names a Density-owned role: the
     * component tier (`sm`, `md`, `lg`), the layout tier (`layout-sm`,
     * `layout-md`, `layout-lg`) or the section tier (`section`), or `none` for contiguous children. Defaults to `layout-md`.
     */
    gap?: StackGap;
    /** The main axis: `column` (the default) or `row`. */
    direction?: StackDirection;
    /** Cross-axis alignment. Defaults to `stretch`. */
    align?: StackAlign;
    /** Main-axis distribution. Defaults to `start`. */
    justify?: StackJustify;
    /** Wrap children onto new lines when they run out of room. */
    wrap?: boolean;
    /** Extra classes merged onto the flow. */
    class?: string;
    /** The items the flow arranges, spaced by `gap`. */
    children?: Snippet;
  }

  const {
    gap = "layout-md",
    direction = "column",
    align = "stretch",
    justify = "start",
    wrap = false,
    class: className,
    children,
    ...rest
  }: Props = $props();
</script>

<div
  {...(rest as Record<string, unknown>)}
  data-stack
  data-stack-direction={direction}
  class={stack({ gap, direction, align, justify, wrap, class: className })}
>
  {@render children?.()}
</div>
