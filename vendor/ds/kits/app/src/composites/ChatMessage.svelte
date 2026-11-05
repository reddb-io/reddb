<!-- One authored message composed from canonical Avatar and MediaObject contracts. -->
<script lang="ts">
  import type { ChatMessageContract } from "../core/contracts/chatmessage";
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import { Avatar, MediaObject } from "@reddb-io/design-system/base";
  import { chatMessage } from "./chat-message.variants";

  interface Props extends ChatMessageContract, Omit<HTMLAttributes<HTMLElement>, keyof ChatMessageContract | "children" | "class"> {
    /** Caller-owned message body. */
    children: Snippet;
    /** Optional caller-owned destinations and controls after the body. */
    actions?: Snippet;
    /** Extra classes, merged over the root slot's own, onto the `article` element. */
    class?: string;
  }

  const {
    author,
    avatarSrc,
    avatarFallback,
    datetime,
    time,
    children,
    actions,
    class: className,
    ...rest
  }: Props = $props();
  const styles = $derived(chatMessage());
  const label = $derived(rest["aria-label"] ?? `Message from ${author}`);
</script>

<article
  {...rest}
  data-chat-message
  aria-label={label}
  class={styles.root({ class: className })}
>
  <MediaObject contentClass={styles.content()}>
    {#snippet media()}
      <Avatar src={avatarSrc} name={author} fallback={avatarFallback} />
    {/snippet}

    <header data-chat-message-header class={styles.header()}>
      <span data-chat-message-author class={styles.author()}>{author}</span>
      <time {datetime} class={styles.time()}>{time}</time>
    </header>
    <div data-chat-message-body class={styles.body()}>{@render children()}</div>
    {#if actions}
      <footer data-chat-message-actions class={styles.actions()}>{@render actions()}</footer>
    {/if}
  </MediaObject>
</article>
