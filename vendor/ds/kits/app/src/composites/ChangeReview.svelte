<script module lang="ts">
  export type { ReviewChange, ReviewState } from "../core/change-review";
</script>

<script lang="ts">
  import { Button, StatusIndicator } from "@reddb-io/design-system/base";
  import { changeReview } from "../change-review.variants";
  import { reviewChanges } from "../core/change-review";
  import type { ChangeReviewContract } from "../core/contracts/changereview";
  interface Props extends ChangeReviewContract {
    /** Additional composition classes. */
    class?: string;
  }
  let {
    title,
    items,
    applying = false,
    blockedReason,
    onapplyrequest,
    ondiscardrequest,
    beforeLabel = "Before",
    afterLabel = "After",
    applyLabel = "Apply changes",
    discardLabel = "Discard all",
    discardItemLabel = "Discard",
    emptyLabel = "No pending changes",
    pendingLabel = "Pending",
    applyingLabel = "Applying",
    appliedLabel = "Applied",
    failedLabel = "Failed",
    headingLevel = 2,
    class: className,
  }: Props = $props();
  const s = changeReview();
  const review = $derived(reviewChanges(items));
  const busy = $derived(applying || review.applying);
  const states = $derived({
    pending: pendingLabel,
    applying: applyingLabel,
    applied: appliedLabel,
    failed: failedLabel,
  });
</script>

<section
  data-change-review
  aria-label={title}
  aria-busy={busy}
  class={s.root({ class: className })}
>
  <svelte:element this={`h${headingLevel}`} class={s.title()}
    >{title}</svelte:element
  >
  <ol class={s.list()}>
    {#each review.items as item (item.id)}
      <li
        data-review-change={item.id}
        data-state={item.state ?? "pending"}
        class={s.item()}
      >
        <div class={s.header()}>
          <span class={s.name()}>{item.label}</span><StatusIndicator
            label={states[item.state ?? "pending"]}
            tone={item.state === "failed"
              ? "danger"
              : item.state === "applied"
                ? "success"
                : "neutral"}
          />
          {#if ondiscardrequest && item.state !== "applied"}<Button
              variant="ghost"
              size="sm"
              disabled={busy}
              aria-label={`${discardItemLabel} ${item.label}`}
              onclick={() => ondiscardrequest?.([item.id])}
              >{discardItemLabel}</Button
            >{/if}
        </div>
        {#if item.description}<p class={s.context()}>{item.description}</p>{/if}
        <dl class={s.comparison()}>
          <div class={s.pair()}>
            <dt class={s.term()}>{beforeLabel}</dt>
            <dd class={s.value()}>{item.before}</dd>
          </div>
          <div class={s.pair()}>
            <dt class={s.term()}>{afterLabel}</dt>
            <dd class={s.value()}>{item.after}</dd>
          </div>
        </dl>
        {#if item.error}<p role="alert" class={s.error()}>{item.error}</p>{/if}
      </li>
    {/each}
  </ol>
  {#if !items.length}<p class={s.context()}>{emptyLabel}</p>{/if}
  {#if blockedReason}<p class={s.context()}>{blockedReason}</p>{/if}
  {#if onapplyrequest || ondiscardrequest}<div class={s.actions()}>
      {#if ondiscardrequest}<Button
          variant="ghost"
          disabled={busy || !review.actionable.length}
          onclick={() => ondiscardrequest?.([...review.actionable])}
          >{discardLabel}</Button
        >{/if}
      {#if onapplyrequest}<Button
          disabled={busy || !!blockedReason || !review.actionable.length}
          loading={busy}
          onclick={() => onapplyrequest?.([...review.actionable])}
          >{applyLabel}</Button
        >{/if}
    </div>{/if}
</section>
