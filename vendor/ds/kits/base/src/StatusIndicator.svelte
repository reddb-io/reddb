<!--
  StatusIndicator — a Base Primitive for status at a glance.

  Colour decorates the status; it never carries it. The required label always
  remains in the document, and showLabel only decides whether sighted readers
  see that text beside the decorative mark.
-->
<script lang="ts">
  import type { HTMLAttributes } from "svelte/elements";
  import { warnDeprecated } from "./deprecation";
  import { statusIndicator } from "./status-indicator.variants";
  import type { Tone } from "./tone";

  interface Props extends Omit<HTMLAttributes<HTMLSpanElement>, "class"> {
    /** Consumer-owned status text; this is the semantic carrier. */
    label: string;
    /** What the status means: the shared tone vocabulary (ADR 0026). Defaults to `neutral`. */
    tone?: Tone;
    /** @deprecated Renamed `tone` (ADR 0026); removed next release. */
    status?: Tone;
    /** Draw the label beside the mark. Hidden labels remain screen-reader text. */
    showLabel?: boolean;
    /** Extra classes, merged over the root slot. */
    class?: string;
  }

  const {
    label,
    tone,
    status,
    showLabel = true,
    class: className,
    ...rest
  }: Props = $props();
  const resolvedTone = $derived<Tone>(tone ?? status ?? "neutral");
  const slots = $derived(statusIndicator({ tone: resolvedTone, labelled: showLabel }));

  $effect(() => {
    if (status !== undefined) warnDeprecated("StatusIndicator", `status="${status}"`, `tone="${status}"`);
  });
</script>

<span
  {...rest}
  class={slots.root({ class: className })}
  data-status-indicator
  data-tone={resolvedTone}
  data-status={resolvedTone}
>
  <span class={slots.mark()} data-status-mark aria-hidden="true"></span>
  <span class={slots.label()} data-status-label>{label}</span>
</span>
