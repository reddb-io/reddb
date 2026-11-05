<script lang="ts">
  import { tick, type Snippet } from "svelte";
  import { Button } from "@reddb-io/design-system/base";
  import { errorBoundary } from "../error-boundary.variants";
  import type { ErrorBoundaryContract } from "../core/contracts/errorboundary";
  interface Props extends ErrorBoundaryContract {
    /** Native child region; client render failures are contained here. */
    children?: Snippet;
    /** Optional caller-owned safe recovery content, receiving the error and retry operation. */
    fallback?: Snippet<[{ error: unknown; retry: () => void }]>;
    /** Additional classes on the failed region. */
    class?: string;
  }
  let {
    title = "This section could not be displayed",
    description = "Try again to recover this section.",
    retryLabel = "Try again",
    resetKey,
    onfailure,
    onretry,
    children,
    fallback,
    class: className,
  }: Props = $props();
  const s = errorBoundary();
</script>

{#key resetKey}
  <svelte:boundary onerror={(error) => onfailure?.(error)}>
    {@render children?.()}
    {#snippet failed(error, reset)}
      {@const retry = () => {
        onretry?.();
        void tick().then(reset);
      }}
      <section
        data-error-boundary
        role="alert"
        aria-label={title}
        class={s.root({ class: className })}
      >
        {#if fallback}{@render fallback({ error, retry })}{:else}
          <p class={s.title()}>{title}</p>
          <p class={s.description()}>{description}</p>
          <Button onclick={retry}>{retryLabel}</Button>
        {/if}
      </section>
    {/snippet}
  </svelte:boundary>
{/key}
