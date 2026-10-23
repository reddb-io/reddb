<!--
  Badge — a Base Primitive for a short status marker inside running text.

  Meaning is `tone` (the shared vocabulary, ADR 0026) and emphasis is
  `variant`; neither is the whole message, so caller-owned text is required
  and always rendered. The span has no interaction of its own; a pressed or
  linked badge is the corresponding native control wearing the exported
  badge classes.
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import {
    badge,
    type BadgeVariant,
    type DeprecatedBadgeVariant,
  } from "./badge.variants";
  import { warnDeprecated } from "./deprecation";
  import type { Tone } from "./tone";

  interface Props extends Omit<HTMLAttributes<HTMLSpanElement>, "class"> {
    /** Status text; required so colour can never be the whole message. */
    children: Snippet;
    /** What the status means. Defaults to `neutral`. */
    tone?: Tone;
    /**
     * How strongly the tone is drawn: `tinted` (default), `filled` or
     * `outline`. `neutral` and `primary` are deprecated for one release.
     */
    variant?: BadgeVariant | DeprecatedBadgeVariant;
    /** Extra classes, merged over the canonical appearance. */
    class?: string;
  }

  const {
    tone = "neutral",
    variant = "tinted",
    class: className,
    children,
    ...rest
  }: Props = $props();

  $effect(() => {
    if (variant === "neutral") warnDeprecated("Badge", 'variant="neutral"', 'variant="tinted" (the default)');
    if (variant === "primary") {
      warnDeprecated(
        "Badge",
        'variant="primary"',
        'a tone with variant="filled" (tone="danger" for an error); the accent fill is not a status',
      );
    }
  });
</script>

<span
  {...rest}
  class={badge({ tone, variant, class: className })}
  data-badge
  data-tone={tone}
  data-variant={variant}
>
  {@render children()}
</span>
