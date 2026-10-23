<!--
  Pill — a Primitive: it imports no other Kit component.

  Optionally dismissible. The dismiss affordance is a plain <button> rather
  than the Kit's Button: reaching for Button here would make Pill a Composite
  and drag a whole control's variants into a 20px target that only ever needs
  to inherit the Pill's own colour. The mechanical Taxonomy test is what keeps
  that decision honest rather than aesthetic.
-->
<script lang="ts">
  import type { PillContract } from "../core/contracts/pill";
  import type { Tone } from "@reddb-io/design-system/base";
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import { quietControl, warnDeprecated } from "@reddb-io/design-system/base";
  import {
    pill,
    pillDismiss,
    type DeprecatedPillVariant,
    type PillSize,
    type PillVariant,
  } from "./pill.variants";

  interface Props extends PillContract, Omit<HTMLAttributes<HTMLSpanElement>, keyof PillContract | "class"> {
    /**
     * When given, the Pill renders a dismiss affordance and calls this. Absent,
     * it renders none — a Pill nobody can remove should not pretend otherwise.
     */
    ondismiss?: () => void;
    /** @deprecated Renamed `ondismiss` (ADR 0026); removed next release. */
    onDismiss?: () => void;
    /** Extra classes, merged over the variant's own. */
    class?: string;
    /** The Pill's label content, rendered before the dismiss affordance. */
    children?: Snippet;
  }

  const {
    tone = "neutral",
    variant = "tinted",
    size = "md",
    ondismiss,
    onDismiss,
    dismissLabel = "Remove",
    class: className,
    children,
    ...rest
  }: Props = $props();

  const dismiss = $derived(ondismiss ?? onDismiss);

  $effect(() => {
    if (onDismiss) warnDeprecated("Pill", "onDismiss", "ondismiss");
    if (variant === "neutral") warnDeprecated("Pill", 'variant="neutral"', 'variant="tinted" (the default)');
    if (variant === "primary") {
      warnDeprecated(
        "Pill",
        'variant="primary"',
        'a tone with variant="filled"; the accent fill is not a meaning',
      );
    }
  });
</script>

<span
  {...rest}
  class={pill({ tone, variant, size, class: className })}
  data-pill
  data-tone={tone}
  data-variant={variant}
>
  {@render children?.()}
  {#if dismiss}
    <button type="button" data-pill-dismiss class={quietControl({ ink: "inherit", class: pillDismiss() })} aria-label={dismissLabel} onclick={() => dismiss()}>
      <!-- A glyph, not an icon dependency: the Kit ships no icon set yet, and
           inventing one inside a Primitive would be a second decision hiding
           inside this slice. -->
      <span aria-hidden="true">&times;</span>
    </button>
  {/if}
</span>
