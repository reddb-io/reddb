<!-- A universal bounded surface whose section spacing follows local Density. -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import Icon, { type IconGlyph } from "./Icon.svelte";
  import Image, { type ImageAsset } from "./Image.svelte";
  import LoadingIndicator from "./LoadingIndicator.svelte";
  import {
    card,
    CARD_RATIO_VALUES,
    type CardActionsAlign,
    type CardMediaFit,
    type CardMediaPosition,
    type CardMediaRatio,
    type CardMediaSpan,
    type CardOrientation,
    type CardPadding,
    type CardTone,
    type CardVariant,
    type DeprecatedCardTone,
  } from "./card.variants";
  import { warnDeprecated } from "./deprecation";

  interface Props extends Omit<HTMLAttributes<HTMLDivElement>, "class" | "title"> {
    /** Edge treatment. Defaults to `outline`. */
    variant?: CardVariant;
    /** Lift the Card onto the raised elevation level. */
    raised?: boolean;
    /** Retain the Card content while announcing background work. */
    busy?: boolean;
    /** Text announced while `busy`, naming what is loading. Defaults to `Loading card content`. */
    busyLabel?: string;
    /** Hides the busy text visually while keeping it announced to assistive technology. */
    busyLabelHidden?: boolean;
    /**
     * What the Card means: the shared tone vocabulary (ADR 0026), each on its
     * Feedback Role's surface and paired readable foreground. Defaults to
     * `neutral`. `brand` is deprecated for one release.
     */
    tone?: CardTone | DeprecatedCardTone;
    /** Density-owned inset applied to every section. Defaults to `md`. */
    padding?: CardPadding;
    /** Main-axis alignment for footer actions. Defaults to `start`. */
    actionsAlign?: CardActionsAlign;
    /** Media/content arrangement. Defaults to `vertical`. */
    orientation?: CardOrientation;
    /** How media fills its ratio-bound region without distortion. Defaults to `cover`. */
    fit?: CardMediaFit;
    /** Focal position retained when covered media crops. Defaults to `center`. */
    position?: CardMediaPosition;
    /** Aspect ratio of the media region. Defaults to `16/9`. */
    ratio?: CardMediaRatio;
    /** Media width or layering treatment. Defaults to `two-fifths`. */
    span?: CardMediaSpan;
    /** Header title, rendered when no `header` snippet is given. */
    title?: string;
    /** Decorative glyph rendered beside the title through the DS Icon contract. */
    icon?: IconGlyph;
    /** Supporting line under the title. */
    description?: string;
    /** Full control over the header. Replaces `icon`, `title`, and `description`. */
    header?: Snippet;
    /** Caller-owned media rendered as a vertical band, horizontal lead, or background. */
    media?: Snippet;
    /**
     * The media as an image the app's pipeline produced, rendered through the Base Image inside the
     * media region with the Card's `fit` and `position`; ignored when a `media` snippet is given.
     */
    image?: ImageAsset;
    /** The `image` is the page's Largest Contentful Paint: fetched eagerly at high priority. */
    priority?: boolean;
    /** Caller-owned actions or content rendered after the body. */
    footer?: Snippet;
    /** Extra classes merged onto the canonical surface. */
    class?: string;
    /** The Card body, rendered between the header and the footer. */
    children?: Snippet;
  }

  const {
    variant = "outline",
    raised = false,
    busy = false,
    busyLabel = "Loading card content",
    busyLabelHidden = false,
    tone = "neutral",
    padding = "md",
    actionsAlign = "start",
    orientation = "vertical",
    fit = "cover",
    position = "center",
    ratio = "16/9",
    span = "two-fifths",
    title,
    icon,
    description,
    header,
    media,
    image,
    priority = false,
    footer,
    class: className,
    children,
    ...rest
  }: Props = $props();

  const slots = $derived(
    card({ variant, raised, tone, padding, actionsAlign, orientation, fit, position, ratio, span }),
  );
  // The media region's ratio as a number, so an `image` reserves the same box.
  const hasMedia = $derived(Boolean(media ?? image));
  const hasHeader = $derived(Boolean(header ?? icon ?? title ?? description));

  $effect(() => {
    if (tone === "brand") {
      warnDeprecated(
        "Card",
        'tone="brand"',
        'a tone (neutral, info, success, warning, danger); the accent is an emphasis, not a tone',
      );
    }
  });
</script>

{#snippet sections()}
  {#if hasHeader}
    <div data-card-header class={slots.header()}>
      {#if header}
        {@render header()}
      {:else}
        {#if icon}
          <div data-card-title-row class={slots.titleRow()}>
            <Icon {icon} aria-hidden="true" class={slots.icon()} />
            {#if title}<p data-card-title class={slots.title()}>{title}</p>{/if}
          </div>
        {:else if title}
          <p data-card-title class={slots.title()}>{title}</p>
        {/if}
        {#if description}<p data-card-description class={slots.description()}>{description}</p>{/if}
      {/if}
    </div>
  {/if}

  <div data-card-body class={slots.body()}>
    {#if busy}
      <LoadingIndicator
        label={busyLabel}
        labelHidden={busyLabelHidden}
        size="sm"
        class="mb-[var(--reddb-spatial-gap-sm)]"
        data-card-busy
      />
    {/if}
    {@render children?.()}
  </div>

  {#if footer}
    <div data-card-footer class={slots.footer()}>{@render footer()}</div>
  {/if}
{/snippet}

{#snippet mediaRegion()}
  <div
    data-card-media
    data-card-media-fit={fit}
    data-card-media-position={position}
    data-card-media-ratio={ratio}
    data-card-media-span={span}
    class={slots.media()}
  >
    {#if media}
      {@render media()}
    {:else if image}
      <Image
        {...image}
        ratio={image.ratio ?? (ratio === "auto" ? undefined : CARD_RATIO_VALUES[ratio])}
        {fit}
        {position}
        {priority}
        class="size-full"
      />
    {/if}
    {#if span === "background"}
      <span
        data-background-media-scrim="strong"
        aria-hidden="true"
        class="pointer-events-none absolute inset-0 bg-background/80"
      ></span>
    {/if}
  </div>
{/snippet}

<div
  {...(rest as Record<string, unknown>)}
  data-card
  data-raised={raised || undefined}
  data-tone={tone}
  data-card-orientation={orientation}
  aria-busy={busy || undefined}
  class={slots.root({ class: className })}
>
  {#if hasMedia && span === "background"}
    {@render mediaRegion()}
    <div data-card-content class={slots.content()}>{@render sections()}</div>
  {:else if hasMedia}
    <div data-card-layout class={slots.layout()}>
      {@render mediaRegion()}
      <div data-card-content class={slots.content()}>{@render sections()}</div>
    </div>
  {:else}
    {@render sections()}
  {/if}
</div>
