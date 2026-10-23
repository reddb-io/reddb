<!-- The only sanctioned seam between a Kit component and a lucide glyph. -->
<script lang="ts" module>
  import type { LucideProps } from "@lucide/svelte";
  import type { Component } from "svelte";

  /*! @license Built-in menu geometry: Copyright (c) 2026 Lucide Icons and Contributors.
   * Permission to use, copy, modify, and/or distribute this software for any
   * purpose with or without fee is hereby granted, provided that the above
   * copyright notice and this permission notice appear in all copies.
   * THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
   * WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
   * MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
   * ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
   * WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
   * ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
   * OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
   */

  export { ICON_SIZES, ICON_COLORS, type IconSize, type IconColor } from "./core/icon";
  import { iconColor, iconDimension, type IconColor, type IconSize, type DeprecatedIconColor } from "./core/icon";
  import { warnDeprecated } from "./deprecation";

  export type IconGlyphProps = LucideProps;
  export type IconGlyph = Component<IconGlyphProps>;
</script>

<script lang="ts">
  import type { SVGAttributes } from "svelte/elements";

  interface Props
    extends Omit<
      SVGAttributes<SVGSVGElement>,
      "color" | "height" | "stroke" | "stroke-width" | "width"
    > {
    /** A consumer-supplied Lucide glyph, or the built-in menu glyph used by navigation without loading the optional icon package. */
    icon: IconGlyph | "menu";
    /** Density-responsive DS size. */
    size?: IconSize;
    /**
     * Semantic Theme color; raw color values are deliberately not accepted.
     * `current` inherits the surrounding ink, so a glyph follows its control's
     * hover and selected states or a caller's token-backed text class.
     * `muted` is a deprecated alias that draws in `ink-muted` for one release.
     */
    color?: IconColor | DeprecatedIconColor;
  }

  let {
    icon: Glyph,
    size = "md",
    color = "foreground",
    class: className,
    ...rest
  }: Props = $props();

  const dimension = $derived(iconDimension(size));
  const resolvedColor = $derived(iconColor(color));
  // Keep the public seam exact (`IconGlyph` is Lucide's own props) while
  // preventing Svelte's template checker from expanding the complete SVG
  // attribute union at this dynamic component boundary.
  const RenderGlyph = $derived(Glyph as Component<Record<string, unknown>>);

  $effect(() => {
    if (color === "muted") warnDeprecated("Icon", 'color="muted"', 'color="ink-muted"');
  });
</script>

{#if Glyph === "menu"}
  <!-- Geometry: Lucide menu, @lucide/svelte 1.52.0 (ISC). -->
  <svg {...rest} data-icon width={dimension} height={dimension} viewBox="0 0 24 24" fill="none" stroke={resolvedColor} stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class={className}>
    <path d="M4 5h16" /><path d="M4 12h16" /><path d="M4 19h16" />
  </svg>
{:else}
  <RenderGlyph {...rest} data-icon size={dimension} color={resolvedColor} strokeWidth={2} class={className} />
{/if}
