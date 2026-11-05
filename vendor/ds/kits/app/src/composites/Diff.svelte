<!-- Change rows whose meaning survives when feedback colours do not. -->
<script module lang="ts">
  import type { DiffRow } from "../core/diff";
  export type { DiffRow } from "../core/diff";
</script>

<script lang="ts">
  import type { DiffContract } from "../core/contracts/diff";
  import type { HTMLAttributes } from "svelte/elements";
  import SplitView from "../primitives/SplitView.svelte";
  import { diff, type DiffMode } from "./diff.variants";

  interface Props extends DiffContract, Omit<HTMLAttributes<HTMLDivElement>, "class" | "role"> {
    /** Extra classes merged over the comparison surface. */
    class?: string;
  }

  let {
    label = "Changes",
    rows,
    mode = "inline",
    beforeLabel = "Before",
    afterLabel = "After",
    separatorLabel = "Resize before and after panes",
    class: className,
    ...rest
  }: Props = $props();
  const styles = $derived(diff());
</script>

{#snippet beforePane()}
  <section
    data-diff-pane="before"
    aria-label={beforeLabel}
    class={styles.pane()}
  >
    <div data-diff-pane-heading aria-hidden="true" class={styles.heading()}>{beforeLabel}</div>
    <div role="list" class={styles.lines()}>
      {#each rows as row}
        {#if row.before === undefined}
          <div data-diff-line data-change-kind="empty" aria-hidden="true" class={styles.line({ change: "empty" })}>
            <span data-diff-marker class={styles.marker()}></span><code></code>
          </div>
        {:else if row.before === row.after}
          <div data-diff-line data-change-kind="context" role="listitem" class={styles.line({ change: "context" })}>
            <span data-diff-marker aria-hidden="true" class={styles.marker()}></span><code>{row.before}</code>
          </div>
        {:else}
          <div
            data-diff-line
            data-change-kind="removal"
            role="listitem"
            aria-label={`Removed: ${row.before}`}
            class={styles.line({ change: "removal" })}
          >
            <span data-diff-marker aria-hidden="true" class={styles.marker()}>−</span><code>{row.before}</code>
          </div>
        {/if}
      {/each}
    </div>
  </section>
{/snippet}

{#snippet afterPane()}
  <section
    data-diff-pane="after"
    aria-label={afterLabel}
    class={styles.pane()}
  >
    <div data-diff-pane-heading aria-hidden="true" class={styles.heading()}>{afterLabel}</div>
    <div role="list" class={styles.lines()}>
      {#each rows as row}
        {#if row.after === undefined}
          <div data-diff-line data-change-kind="empty" aria-hidden="true" class={styles.line({ change: "empty" })}>
            <span data-diff-marker class={styles.marker()}></span><code></code>
          </div>
        {:else if row.before === row.after}
          <div data-diff-line data-change-kind="context" role="listitem" class={styles.line({ change: "context" })}>
            <span data-diff-marker aria-hidden="true" class={styles.marker()}></span><code>{row.after}</code>
          </div>
        {:else}
          <div
            data-diff-line
            data-change-kind="addition"
            role="listitem"
            aria-label={`Added: ${row.after}`}
            class={styles.line({ change: "addition" })}
          >
            <span data-diff-marker aria-hidden="true" class={styles.marker()}>+</span><code>{row.after}</code>
          </div>
        {/if}
      {/each}
    </div>
  </section>
{/snippet}

<div
  {...rest}
  data-diff
  data-mode={mode}
  role="region"
  aria-label={label}
  class={styles.root({ class: className })}
>
  {#if mode === "side-by-side"}
    <SplitView
      start={beforePane}
      end={afterPane}
      label={separatorLabel}
      class={styles.split()}
    />
  {:else}
    <div role="list" class={styles.lines()}>
      {#each rows as row}
        {#if row.before !== undefined && row.before === row.after}
          <div data-diff-line data-change-kind="context" role="listitem" class={styles.line({ change: "context" })}>
            <span data-diff-marker aria-hidden="true" class={styles.marker()}></span><code>{row.before}</code>
          </div>
        {:else}
          {#if row.before !== undefined}
            <div
              data-diff-line
              data-change-kind="removal"
              role="listitem"
              aria-label={`Removed: ${row.before}`}
              class={styles.line({ change: "removal" })}
            >
              <span data-diff-marker aria-hidden="true" class={styles.marker()}>−</span><code>{row.before}</code>
            </div>
          {/if}
          {#if row.after !== undefined}
            <div
              data-diff-line
              data-change-kind="addition"
              role="listitem"
              aria-label={`Added: ${row.after}`}
              class={styles.line({ change: "addition" })}
            >
              <span data-diff-marker aria-hidden="true" class={styles.marker()}>+</span><code>{row.after}</code>
            </div>
          {/if}
        {/if}
      {/each}
    </div>
  {/if}
</div>
