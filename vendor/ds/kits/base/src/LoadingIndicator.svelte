<script lang="ts">
  import type { HTMLAttributes } from "svelte/elements";
  import {
    loadingIndicator,
    type LoadingIndicatorSize,
  } from "./loading-indicator.variants";

  interface Props extends Omit<HTMLAttributes<HTMLDivElement>, "class"> {
    /** What is being waited for. */
    label?: string;
    /** Spinner and label size: `sm`, `md` (the default) or `lg`. */
    size?: LoadingIndicatorSize;
    /** Keep the announcement while visually hiding its sentence. */
    labelHidden?: boolean;
    /** Extra classes merged onto the indicator root. */
    class?: string;
  }

  const {
    label = "Loading…",
    size = "md",
    labelHidden = false,
    class: className,
    ...rest
  }: Props = $props();

  const slots = $derived(loadingIndicator({ size, labelHidden }));
</script>

<div
  {...(rest as Record<string, unknown>)}
  data-loading-indicator
  class={slots.root({ class: className })}
  role="status"
  aria-live="polite"
  aria-busy="true"
>
  <svg class={slots.spinner()} viewBox="0 0 24 24" aria-hidden="true">
    <circle class={slots.track()} cx="12" cy="12" r="9" stroke-width="3" />
    <path
      class={slots.head()}
      d="M21 12a9 9 0 0 0-9-9"
      stroke-width="3"
      stroke-linecap="round"
    />
  </svg>
  <span class={slots.label()}>{label}</span>
</div>
