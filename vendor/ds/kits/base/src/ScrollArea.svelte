<!-- A named native overflow region that can always receive keyboard focus. -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import { scrollArea, type ScrollAreaOrientation } from "./scroll-area.variants";

  interface Props extends Omit<HTMLAttributes<HTMLDivElement>, "aria-label" | "children" | "class" | "role" | "tabindex"> {
    /** Accessible region name, required because this is a keyboard stop. */
    label: string;
    /** Which native overflow directions this boundary contains. */
    orientation?: ScrollAreaOrientation;
    /** Surface treatment: `outline` keeps the standalone frame and Density inset; `plain` embeds native scrolling in an existing panel. Defaults to `outline`. */
    variant?: "outline" | "plain";
    /** Extra classes, including the consumer-owned size constraint. */
    class?: string;
    /** Content that scrolls inside the region. */
    children?: Snippet;
  }

  const {
    label,
    orientation = "vertical",
    variant = "outline",
    class: className,
    children,
    ...rest
  }: Props = $props();
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex (a named overflow region must be keyboard-scrollable) -->
<div
  {...rest}
  data-scroll-area
  role="region"
  aria-label={label}
  tabindex="0"
  class={scrollArea({ orientation, variant, class: className })}
>
  {@render children?.()}
</div>
