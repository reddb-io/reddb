<!-- A decorative phone body whose viewport belongs entirely to the caller. -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import AspectRatio from "./AspectRatio.svelte";
  import { DEFAULT_PHONE_MOCKUP_RATIO, phoneMockup } from "./phone-mockup.variants";

  interface Props
    extends Omit<
      HTMLAttributes<HTMLDivElement>,
      "aria-hidden" | "aria-label" | "children" | "class" | "role"
    > {
    /** Width divided by viewport height. */
    ratio?: number;
    /** Extra classes merged onto the decorative phone body. */
    class?: string;
    /** Caller-owned screenshot, live embed, code sample, or other content. */
    children: Snippet;
  }

  const {
    ratio = DEFAULT_PHONE_MOCKUP_RATIO,
    class: className,
    children,
    ...rest
  }: Props = $props();
  const styles = $derived(phoneMockup());
</script>

<div
  {...(rest as Record<string, unknown>)}
  data-phone-mockup
  role="presentation"
  class={styles.root({ class: className })}
>
  <div data-phone-mockup-chrome aria-hidden="true" class={styles.chrome()}>
    <span class={styles.speaker()}></span>
  </div>
  <AspectRatio data-phone-mockup-viewport {ratio} class={styles.viewport()}>
    {@render children()}
  </AspectRatio>
</div>
