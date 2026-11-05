<!-- A heading whose outline level and type role are separate decisions (ADR 0025). -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import {
    defaultHeadingRole,
    heading,
    type HeadingLevel,
    type HeadingRole,
  } from "./heading.variants";

  interface Props extends Omit<HTMLAttributes<HTMLElement>, "class" | "role"> {
    /** Document outline depth: renders `h1`…`h6`. */
    level?: HeadingLevel;
    /**
     * The Theme type role it reads as — independent of `level`. Defaults to
     * `title` at level 1 and `heading` below it.
     */
    role?: HeadingRole;
    /** Extra classes merged onto the heading element. */
    class?: string;
    /** The heading text or inline content. */
    children: Snippet;
  }

  const { level = 2, role, class: className, children, ...rest }: Props = $props();

  const resolvedRole = $derived(role ?? defaultHeadingRole(level));
  const classes = $derived(heading({ role: resolvedRole, class: className }));
</script>

<!-- One static element per level, never `<svelte:element>`: before Svelte 5.57.2
     a dynamic element is detached and re-inserted while it hydrates, which the
     browser counts as a new paint, so a heading that is the page's Largest
     Contentful Paint would be painted again only once the JavaScript has run. -->
{#if level === 1}<h1 {...rest} data-heading data-type-role={resolvedRole} class={classes}>{@render children()}</h1>
{:else if level === 2}<h2 {...rest} data-heading data-type-role={resolvedRole} class={classes}>{@render children()}</h2>
{:else if level === 3}<h3 {...rest} data-heading data-type-role={resolvedRole} class={classes}>{@render children()}</h3>
{:else if level === 4}<h4 {...rest} data-heading data-type-role={resolvedRole} class={classes}>{@render children()}</h4>
{:else if level === 5}<h5 {...rest} data-heading data-type-role={resolvedRole} class={classes}>{@render children()}</h5>
{:else}<h6 {...rest} data-heading data-type-role={resolvedRole} class={classes}>{@render children()}</h6>{/if}
