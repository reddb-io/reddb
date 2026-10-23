<!-- A compact card title over the canonical section-heading outline contract. -->
<script lang="ts">
  import type { CardHeadingContract } from "../core/contracts/cardheading";
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import {
    SectionHeading,
    type SectionHeadingLevel,
    type SectionHeadingSize,
  } from "@reddb-io/design-system/base";
  import { cardHeading } from "./card-heading.variants";

  interface Props extends CardHeadingContract, Omit<HTMLAttributes<HTMLDivElement>, keyof CardHeadingContract | "class" | "title"> {
    /**
     * Controls rendered at the end of the heading row; they wrap below the title when the row is
     * narrow.
     */
    actions?: Snippet;
    /** Extra classes, merged over the heading's own, onto the root element. */
    class?: string;
  }

  const {
    title,
    description,
    level = 2,
    size = "sm",
    actions,
    class: className,
    ...rest
  }: Props = $props();
</script>

<SectionHeading
  {...(rest as Record<string, unknown>)}
  data-card-heading
  {title}
  {description}
  {level}
  {size}
  rule={false}
  {actions}
  class={cardHeading({ class: className })}
/>
