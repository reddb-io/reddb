<!-- A scored authored review composed from canonical Base surface and media contracts. -->
<script lang="ts">
  import type { ReviewContract } from "../core/contracts/review";
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import { Avatar, Card, MediaObject, Rating } from "@reddb-io/design-system/base";
  import { review } from "./review.variants";

  interface Props extends ReviewContract, Omit<HTMLAttributes<HTMLElement>, keyof ReviewContract | "children" | "class"> {
    /** Caller-owned review body. */
    children: Snippet;
    /** Optional caller-owned destinations and controls. */
    actions?: Snippet;
    /** Extra classes, merged over the root slot's own, onto the `article` element. */
    class?: string;
  }

  const generatedId = $props.id();
  const {
    author,
    avatarSrc,
    avatarFallback,
    rating: score,
    maxRating = 5,
    datetime,
    time,
    children,
    actions,
    class: className,
    ...rest
  }: Props = $props();
  const styles = $derived(review());
  const label = $derived(rest["aria-label"] ?? `Review by ${author}`);
</script>

<article {...rest} data-review aria-label={label} class={styles.root({ class: className })}>
  <Card class={styles.card()}>
    {#snippet header()}
      <MediaObject>
        {#snippet media()}
          <Avatar src={avatarSrc} name={author} fallback={avatarFallback} />
        {/snippet}

        <div class={styles.identity()}>
          <span data-review-author class={styles.author()}>{author}</span>
          {#if datetime !== undefined && time !== undefined}
            <time {datetime} class={styles.time()}>{time}</time>
          {/if}
        </div>
      </MediaObject>
    {/snippet}

    <div data-review-content class={styles.content()}>
      <Rating
        id={`${generatedId}-score`}
        legend={`Rating by ${author}`}
        name={`${generatedId}-score`}
        value={score}
        max={maxRating}
        disabled
      />
      <div data-review-body class={styles.body()}>{@render children()}</div>
      {#if actions}
        <footer data-review-actions class={styles.actions()}>{@render actions()}</footer>
      {/if}
    </div>
  </Card>
</article>
