<!--
  Notification — transient feedback composed from the canonical Alert and
  Button contracts. The consumer owns placement and when to create it; this
  component owns the announcement and an explicit keyboard dismissal path.
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import Alert from "./Alert.svelte";
  import Button from "./Button.svelte";
  import type { AlertFeedbackRole } from "./alert.variants";
  import { warnDeprecated } from "./deprecation";
  import { notification } from "./notification.variants";
  import { feedbackTone } from "./core/feedback";
  import type { Tone } from "./tone";

  interface Props extends Omit<HTMLAttributes<HTMLDivElement>, "class" | "title"> {
    /** What the notification means, announced with Alert's canonical urgency. Defaults to `neutral`. */
    tone?: Tone;
    /** @deprecated Renamed `tone` (ADR 0026); removed next release. */
    feedback?: AlertFeedbackRole;
    /** Required announcement text. */
    title: string;
    /** Optional caller-owned detail. */
    children?: Snippet;
    /** Accessible and visible label for the dismissal control. */
    dismissLabel?: string;
    /** Called after the notification removes itself. */
    ondismiss?: () => void;
    /** Extra classes merged over the notification root. */
    class?: string;
  }

  const {
    tone,
    feedback,
    title,
    children,
    dismissLabel = "Dismiss",
    ondismiss,
    class: className,
    ...rest
  }: Props = $props();

  let visible = $state(true);
  const slots = $derived(notification());
  const resolvedTone = $derived<Tone>(feedbackTone(tone, feedback));

  $effect(() => {
    if (feedback !== undefined) warnDeprecated("Notification", `feedback="${feedback}"`, `tone="${feedback}"`);
  });

  function dismiss(): void {
    visible = false;
    ondismiss?.();
  }
</script>

{#if visible}
  <div
    {...(rest as Record<string, unknown>)}
    data-notification
    class={slots.root({ class: className })}
  >
    <Alert tone={resolvedTone} {title} {children} class={slots.announcement()} />
    <Button
      variant="ghost"
      size="sm"
      class={slots.dismiss()}
      aria-label={dismissLabel}
      onclick={dismiss}
    >{dismissLabel}</Button>
  </div>
{/if}
