<!--
  Alert — a universal Base Primitive for one message and what it means.

  Neutral, info, success and warning are polite status updates. Danger is an
  assertive error alert. The component fixes that semantic behavior while the
  Theme owns the replaceable Brand-material mapping behind each tone.
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import { alert, type AlertFeedbackRole } from "./alert.variants";
  import { warnDeprecated } from "./deprecation";
  import { feedbackTone, feedbackAnnouncement } from "./core/feedback";
  import type { Tone } from "./tone";

  interface Props extends Omit<HTMLAttributes<HTMLDivElement>, "class" | "role" | "title"> {
    /** What the message means: the shared tone vocabulary (ADR 0026). Defaults to `neutral`. */
    tone?: Tone;
    /** @deprecated Renamed `tone` (ADR 0026); removed next release. */
    feedback?: AlertFeedbackRole;
    /** Optional visible heading; this is message content, not the native title tooltip. */
    title?: string;
    /** Extra classes, merged over the canonical tone appearance. */
    class?: string;
    /**
     * The message body, rendered below the optional `title`. Nothing is rendered for it when
     * omitted.
     */
    children?: Snippet;
  }

  const { tone, feedback, title, class: className, children, ...rest }: Props = $props();
  const resolvedTone = $derived<Tone>(feedbackTone(tone, feedback));
  const announcementRole = $derived(feedbackAnnouncement(resolvedTone));

  $effect(() => {
    if (feedback !== undefined) warnDeprecated("Alert", `feedback="${feedback}"`, `tone="${feedback}"`);
  });
</script>

<div
  {...(rest as Record<string, unknown>)}
  role={announcementRole}
  data-tone={resolvedTone}
  class={alert({ tone: resolvedTone, class: className })}
>
  {#if title}<p class="font-medium">{title}</p>{/if}
  {#if children}
    <div class={title ? "mt-[var(--reddb-spatial-gap-sm)]" : undefined}>{@render children()}</div>
  {/if}
</div>
