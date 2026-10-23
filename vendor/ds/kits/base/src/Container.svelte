<!--
  A universal content boundary at a DS-named width (ADR 0021) whose inline
  inset follows local Density.
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import { container, type ContainerSize } from "./container.variants";

  interface Props extends Omit<HTMLAttributes<HTMLDivElement>, "class"> {
    /** The DS-named maximum width: `prose`, `content`, `wide` (the default) or `full`. */
    size?: ContainerSize;
    /** Extra classes merged onto the canonical content boundary. */
    class?: string;
    /** Page or section content held inside the width-limited boundary. */
    children?: Snippet;
  }

  const { size = "wide", class: className, children, ...rest }: Props = $props();
</script>

<div
  {...(rest as Record<string, unknown>)}
  data-container
  data-container-size={size}
  class={container({ size, class: className })}
>
  {@render children?.()}
</div>
