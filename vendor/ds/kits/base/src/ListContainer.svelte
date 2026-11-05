<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import Card from "./Card.svelte";
  import type { CardPadding, CardVariant } from "./card.variants";

  interface Props extends Omit<HTMLAttributes<HTMLDivElement>, "class" | "title"> {
    /** Edge treatment of the underlying Card: `outline` (the default) or `plain`. */
    variant?: CardVariant;
    /** Density-owned inset applied to every Card section. Defaults to `md`. */
    padding?: CardPadding;
    /** Header title, rendered when no `header` snippet is given. */
    title?: string;
    /** Supporting line under the title. */
    description?: string;
    /** Full control over the header. Replaces `title` and `description`. */
    header?: Snippet;
    /** Caller-owned actions or content rendered after the list. */
    footer?: Snippet;
    /** Extra classes merged onto the underlying Card surface. */
    class?: string;
    /** The list itself, rendered in the Card body. */
    children?: Snippet;
  }

  let {
    variant = "outline",
    padding = "md",
    title,
    description,
    header,
    footer,
    class: className,
    children,
    ...rest
  }: Props = $props();
</script>

<Card
  {...rest}
  data-list-container
  {variant}
  {padding}
  {title}
  {description}
  {header}
  {footer}
  class={className}
>
  {@render children?.()}
</Card>
