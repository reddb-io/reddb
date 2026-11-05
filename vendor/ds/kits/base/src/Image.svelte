<!--
  A picture that reserves its box, names its alternative and loads at the
  priority the caller states. The DS never resizes or re-encodes: the
  application's asset pipeline writes the variants, and the Image accepts the
  URLs (srcset, picture sources) and the intrinsic size that pipeline produced.
-->
<script module lang="ts">
  export type { ImageAsset, ImageFit, ImagePosition, ImageSource } from "./image.variants";
</script>

<script lang="ts">
  import type { HTMLImgAttributes } from "svelte/elements";
  import {
    assertImageContract,
    image,
    imageRatio,
    type ImageFit,
    type ImagePosition,
    type ImageSource,
  } from "./image.variants";

  interface Props
    extends Omit<
      HTMLImgAttributes,
      | "alt"
      | "class"
      | "decoding"
      | "fetchpriority"
      | "height"
      | "loading"
      | "sizes"
      | "src"
      | "srcset"
      | "style"
      | "width"
    > {
    /** Fallback URL, used when no `srcset` candidate or `source` applies. */
    src: string;
    /**
     * Text alternative, required. `alt=""` is refused unless `decorative` says the image carries no
     * information.
     */
    alt: string;
    /** The image carries no information: renders `alt=""`, and `alt` must be empty. */
    decorative?: boolean;
    /** Intrinsic width in px of `src`; with `height` it reserves the box before the bytes arrive. */
    width?: number;
    /** Intrinsic height in px of `src`; with `width` it reserves the box before the bytes arrive. */
    height?: number;
    /**
     * Width divided by height, reserved when the intrinsic size is unknown (or to crop to another
     * shape with `fit`). One of `ratio` or `width` and `height` is required.
     */
    ratio?: number;
    /** Width- or density-described candidates of the same picture, from the app's pipeline. */
    srcset?: string;
    /** Slot widths the browser uses to pick a `w`-described candidate. */
    sizes?: string;
    /**
     * `<picture>` sources tried in order before the `img` — a modern format (AVIF, then WebP) or an
     * art-directed crop. Omitted, a bare `img` renders.
     */
    sources?: readonly ImageSource[];
    /**
     * The Largest Contentful Paint candidate: fetched at high priority, eagerly, and decoded
     * synchronously. Defaults to `false` — lazy loading and async decoding.
     */
    priority?: boolean;
    /** Overrides the loading strategy `priority` implies; rarely needed. */
    loading?: "lazy" | "eager";
    /** How the picture fills its reserved box: `cover` (default) crops, `contain` letterboxes. */
    fit?: ImageFit;
    /** The focal point kept in frame when `cover` crops. Defaults to `center`. */
    position?: ImagePosition;
    /** Extra classes merged onto the `img`. */
    class?: string;
  }

  const {
    src,
    alt,
    decorative = false,
    width,
    height,
    ratio,
    srcset,
    sizes,
    sources = [],
    priority = false,
    loading,
    fit = "cover",
    position = "center",
    class: className,
    ...rest
  }: Props = $props();

  const box = $derived.by(() => {
    assertImageContract({ alt, decorative, width, height, ratio });
    return imageRatio({ width, height, ratio });
  });
  const classes = $derived(image({ fit, position, class: className }));
</script>

{#snippet img()}
  <img
    {...rest}
    {src}
    alt={decorative ? "" : alt}
    {width}
    {height}
    {srcset}
    {sizes}
    loading={loading ?? (priority ? "eager" : "lazy")}
    decoding={priority ? "sync" : "async"}
    fetchpriority={priority ? "high" : undefined}
    style:aspect-ratio={String(box)}
    data-image
    data-image-priority={priority || undefined}
    class={classes}
  />
{/snippet}

{#if sources.length > 0}
  <!-- The picture adds no box of its own: layout and fit stay the img's. -->
  <picture data-image-picture class="contents">
    {#each sources as source (source.srcset)}
      <source srcset={source.srcset} type={source.type} media={source.media} sizes={source.sizes ?? sizes} />
    {/each}
    {@render img()}
  </picture>
{:else}
  {@render img()}
{/if}
