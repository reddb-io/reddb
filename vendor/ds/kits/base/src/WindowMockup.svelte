<!-- Plain decorative window chrome around caller-owned, semantically intact content. -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import AspectRatio from "./AspectRatio.svelte";
  import { DEFAULT_WINDOW_MOCKUP_RATIO, windowMockup } from "./window-mockup.variants";

  interface Props
    extends Omit<
      HTMLAttributes<HTMLDivElement>,
      "aria-hidden" | "aria-label" | "children" | "class" | "role" | "title"
    > {
    /** Optional display-only title for the decorative title bar. */
    title?: string;
    /** Width divided by viewport height. */
    ratio?: number;
    /** Extra classes merged onto the decorative frame. */
    class?: string;
    /** Caller-owned screenshot, live embed, code sample, or other content. */
    children: Snippet;
  }

  const {
    title,
    ratio = DEFAULT_WINDOW_MOCKUP_RATIO,
    class: className,
    children,
    ...rest
  }: Props = $props();
  const styles = $derived(windowMockup());
</script>

<div
  {...(rest as Record<string, unknown>)}
  data-window-mockup
  role="presentation"
  class={styles.root({ class: className })}
>
  <div data-window-mockup-chrome aria-hidden="true" class={styles.chrome()}>
    <span class={styles.controls()}>
      <span class={styles.control()}></span>
      <span class={styles.control()}></span>
      <span class={styles.control()}></span>
    </span>
    <span class={styles.title()}>{title ?? ""}</span>
    <span class={styles.balance()}></span>
  </div>
  <AspectRatio data-window-mockup-viewport {ratio} class={styles.viewport()}>
    {@render children()}
  </AspectRatio>
</div>
