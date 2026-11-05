<!-- A structural media boundary that leaves content and every appearance axis to its caller. -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import { aspectRatio, DEFAULT_ASPECT_RATIO } from "./aspect-ratio.variants";

  interface Props extends Omit<HTMLAttributes<HTMLDivElement>, "class"> {
    /** Width divided by height. Any positive ratio is supported. */
    ratio?: number;
    /** Extra classes merged onto the structural boundary. */
    class?: string;
    /** Media or other content placed inside the ratio-bound boundary. */
    children?: Snippet;
  }

  const {
    ratio = DEFAULT_ASPECT_RATIO,
    class: className,
    style,
    children,
    ...rest
  }: Props = $props();
</script>

<div
  {...(rest as Record<string, unknown>)}
  data-aspect-ratio
  style={`${style ?? ""}; aspect-ratio: ${ratio}`}
  class={aspectRatio({ class: className })}
>
  {@render children?.()}
</div>
