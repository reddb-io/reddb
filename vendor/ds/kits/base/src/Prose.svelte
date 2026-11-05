<!--
  Rendered long-form HTML — CommonMark and GFM — on the Theme's type roles
  (ADR 0025, 0027). It styles its children from the outside, so a Markdown
  renderer's output (or `{@html}` the caller has sanitised) reads as a document.
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import { prose, type ProseElement, type ProseMeasure } from "./prose.variants";

  interface Props extends Omit<HTMLAttributes<HTMLElement>, "class"> {
    /** The reading width, on DS-named widths: `prose` (65ch of the body size, the default), `content` or `full`. */
    measure?: ProseMeasure;
    /** Tint every other body row of the document's tables, for long tables read across. */
    striped?: boolean;
    /** The element rendered: `div` (default), `article` or `section`. */
    as?: ProseElement;
    /** Extra classes merged onto the document root, such as the caller's own inset. */
    class?: string;
    /** The rendered document: unclassed CommonMark or GFM markup, or sanitised `{@html}`. */
    children?: Snippet;
  }

  const {
    measure = "prose",
    striped = false,
    as = "div",
    class: className,
    children,
    ...rest
  }: Props = $props();
</script>

<svelte:element
  this={as}
  {...rest}
  data-prose
  data-prose-measure={measure}
  class={prose({ measure, striped, class: className })}
>{@render children?.()}</svelte:element>
