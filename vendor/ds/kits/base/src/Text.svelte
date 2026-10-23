<!-- Running copy in a Theme reading role: body or caption (ADR 0025). -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import { warnDeprecated } from "./deprecation";
  import {
    defaultTextInk,
    text,
    type TextElement,
    type TextInk,
    type TextRole,
  } from "./text.variants";

  interface Props extends Omit<HTMLAttributes<HTMLElement>, "class" | "role"> {
    /** The Theme reading role. */
    role?: TextRole;
    /** Ink or secondary ink; a caption defaults to muted. */
    ink?: TextInk;
    /** @deprecated Renamed `ink` (ADR 0026: `tone` is the semantic vocabulary); removed next release. */
    tone?: TextInk;
    /** The element rendered: `p` (default), `span`, `div` or `small`. */
    as?: TextElement;
    /** Extra classes merged onto the rendered element, over its role styles. */
    class?: string;
    /** The copy rendered inside the element. */
    children: Snippet;
  }

  const { role = "body", ink, tone, as = "p", class: className, children, ...rest }: Props = $props();

  // `ink` wins; the deprecated `tone` is honoured for one release.
  const resolvedInk = $derived(ink ?? tone ?? defaultTextInk(role));
  const classes = $derived(text({ role, ink: resolvedInk, class: className }));
  $effect(() => {
    if (tone !== undefined) warnDeprecated("Text", `tone="${tone}"`, `ink="${tone}"`);
  });
</script>

<!-- Static elements, never `<svelte:element>`: see Heading — a dynamic element
     is re-inserted while it hydrates (before Svelte 5.57.2), repainting a lede
     that is the page's Largest Contentful Paint. -->
{#if as === "span"}<span {...rest} data-text data-type-role={role} class={classes}>{@render children()}</span>
{:else if as === "div"}<div {...rest} data-text data-type-role={role} class={classes}>{@render children()}</div>
{:else if as === "small"}<small {...rest} data-text data-type-role={role} class={classes}>{@render children()}</small>
{:else}<p {...rest} data-text data-type-role={role} class={classes}>{@render children()}</p>{/if}
