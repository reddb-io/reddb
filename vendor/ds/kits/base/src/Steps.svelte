<script module lang="ts">
  export type { StepItem, StepState } from "./core/steps";
</script>

<script lang="ts">
  import type { StepItem, StepState } from "./core/steps";
  import Link from "./Link.svelte";
  import { steps } from "./steps.variants";

  interface Props {
    /**
     * The steps in order; each has an `id`, a `label`, a `state` of `complete`, `current` or
     * `upcoming`, and an optional `href` that makes a non-current step a link.
     */
    items: readonly StepItem[];
    /** Accessible name of the navigation landmark; defaults to `Progress`. */
    label?: string;
    /** Extra classes, merged over the root slot's own. */
    class?: string;
  }

  let { items, label = "Progress", class: className }: Props = $props();
  const styles = steps();
</script>

<nav aria-label={label} data-steps class={styles.root({ class: className })}>
  <ol class={styles.list()}>
    {#each items as item, index (item.id)}
      <li data-step-state={item.state} class={styles.item()}>
        {#if item.state === "current"}
          <span aria-current="step" class={styles.content()}>
            <span aria-hidden="true" class={styles.marker()}>{index + 1}</span>
            <span class={styles.current()}>{item.label}</span>
          </span>
        {:else if item.href !== undefined}
          <Link href={item.href} class={styles.content()}>
            <span aria-hidden="true" class={styles.marker()}>{item.state === "complete" ? "✓" : index + 1}</span>
            <span>{item.label}</span>
            {#if item.state === "complete"}<span class={styles.status()}>Completed</span>{/if}
          </Link>
        {:else}
          <span class={styles.content()}>
            <span aria-hidden="true" class={styles.marker()}>{item.state === "complete" ? "✓" : index + 1}</span>
            <span>{item.label}</span>
            {#if item.state === "complete"}<span class={styles.status()}>Completed</span>{/if}
          </span>
        {/if}
      </li>
    {/each}
  </ol>
</nav>
