<!-- A generic media/content relationship with no page or business vocabulary. -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import {
    mediaObject,
    type MediaObjectAlign,
    type MediaObjectGap,
  } from "./media-object.variants";

  interface Props extends Omit<HTMLAttributes<HTMLDivElement>, "children" | "class"> {
    /** Caller-owned leading media, commonly an Avatar or image. */
    media: Snippet;
    /** Caller-owned heading, metadata, actions, and body. */
    children: Snippet;
    /** Cross-axis alignment of media and content. */
    align?: MediaObjectAlign;
    /** Density-owned separation between media and content. */
    gap?: MediaObjectGap;
    /** Extra classes merged onto the relationship root. */
    class?: string;
    /** Extra classes merged onto the wrapper around the `media` snippet, over its own. */
    mediaClass?: string;
    /** Extra classes merged onto the wrapper around `children`, over its own. */
    contentClass?: string;
  }

  const {
    media,
    children,
    align = "start",
    gap = "md",
    class: className,
    mediaClass,
    contentClass,
    ...rest
  }: Props = $props();
  const styles = $derived(mediaObject({ align, gap }));
</script>

<div {...rest} data-media-object class={styles.root({ class: className })}>
  <div data-media-object-media class={styles.media({ class: mediaClass })}>{@render media()}</div>
  <div data-media-object-content class={styles.content({ class: contentClass })}>
    {@render children()}
  </div>
</div>
