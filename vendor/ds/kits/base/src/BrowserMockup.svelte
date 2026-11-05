<!-- Decorative browser chrome around caller-owned, semantically intact content. -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import AspectRatio from "./AspectRatio.svelte";
  import {
    browserMockup,
    DEFAULT_BROWSER_MOCKUP_RATIO,
  } from "./browser-mockup.variants";

  interface Props
    extends Omit<
      HTMLAttributes<HTMLDivElement>,
      "aria-hidden" | "aria-label" | "children" | "class" | "role"
    > {
    /** Optional display-only address. It is never rendered as a link or control. */
    address?: string;
    /** Width divided by viewport height. */
    ratio?: number;
    /** Extra classes merged onto the decorative frame. */
    class?: string;
    /** Caller-owned screenshot, live embed, code sample, or other content. */
    children: Snippet;
  }

  const {
    address,
    ratio = DEFAULT_BROWSER_MOCKUP_RATIO,
    class: className,
    children,
    ...rest
  }: Props = $props();
  const styles = $derived(browserMockup());
</script>

<div
  {...(rest as Record<string, unknown>)}
  data-browser-mockup
  role="presentation"
  class={styles.root({ class: className })}
>
  <div data-browser-mockup-chrome aria-hidden="true" class={styles.chrome()}>
    <span class={styles.controls()}>
      <span class={styles.control()}></span>
      <span class={styles.control()}></span>
      <span class={styles.control()}></span>
    </span>
    <span class={styles.address()}>{address ?? ""}</span>
  </div>
  <AspectRatio data-browser-mockup-viewport {ratio} class={styles.viewport()}>
    {@render children()}
  </AspectRatio>
</div>
