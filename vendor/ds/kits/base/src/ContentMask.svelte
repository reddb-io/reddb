<!-- A presentation-only clip around caller-owned content. The wrapper adds no
     name, role, keyboard stop, or appearance-axis selection of its own. -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import {
    CONTENT_MASK_CLIP_PATHS,
    contentMask,
    type ContentMaskShape,
  } from "./content-mask.variants";

  interface Props extends Omit<HTMLAttributes<HTMLDivElement>, "class"> {
    /** The named geometry that clips the caller-owned content. */
    shape: ContentMaskShape;
    /** Content keeps its own semantics, accessible name, and interaction. */
    children: Snippet;
    /** Extra classes merged onto the structural boundary. */
    class?: string;
  }

  const { shape, children, class: className, style, ...rest }: Props = $props();
</script>

<div
  {...rest}
  data-content-mask
  data-shape={shape}
  style={`${style ?? ""}; clip-path: ${CONTENT_MASK_CLIP_PATHS[shape]}`}
  class={contentMask({ class: className })}
>
  {@render children()}
</div>
