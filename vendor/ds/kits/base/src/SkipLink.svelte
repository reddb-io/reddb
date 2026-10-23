<!-- A keyboard bypass to the document's canonical main-content landmark. -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAnchorAttributes, MouseEventHandler } from "svelte/elements";
  import { focusSkipTarget } from "./core/skip-link";
  import { skipLink } from "./skip-link.variants";

  interface Props extends Omit<HTMLAnchorAttributes, "class"> {
    /** In-page destination. Defaults to the shared main-content landmark. */
    href?: string;
    /** Extra classes merged onto the focused bypass control. */
    class?: string;
    /** Optional local wording; the default is already a complete accessible name. */
    children?: Snippet;
  }

  const {
    href = "#main-content",
    onclick,
    class: className,
    children,
    ...rest
  }: Props = $props();

  const handleClick: MouseEventHandler<HTMLAnchorElement> = (event) => {
    onclick?.(event);
    focusSkipTarget(event, href, event.currentTarget.ownerDocument);
  };
</script>

<a
  {...(rest as Record<string, unknown>)}
  {href}
  data-skip-link
  class={skipLink({ class: className })}
  onclick={handleClick}
>
  {#if children}{@render children()}{:else}Skip to main content{/if}
</a>
