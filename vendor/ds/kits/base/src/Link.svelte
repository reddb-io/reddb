<!-- A native link whose underline keeps it distinguishable without colour. -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAnchorAttributes } from "svelte/elements";
  import { link } from "./link.variants";

  interface Props extends Omit<HTMLAnchorAttributes, "children" | "class" | "href"> {
    /** Destination of the link. Required: the element is always a native anchor. */
    href: string;
    /** Extra classes merged onto the canonical link appearance. */
    class?: string;
    /** The link text or content. */
    children?: Snippet;
  }

  const { href, class: className, children, ...rest }: Props = $props();
</script>

<a {...rest} {href} data-link class={link({ class: className })}>
  {@render children?.()}
</a>
