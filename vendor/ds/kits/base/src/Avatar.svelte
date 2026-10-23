<!-- A named image with an icon or required text fallback when media is absent or fails. -->
<script lang="ts">
  import type { HTMLAttributes } from "svelte/elements";
  import { untrack } from "svelte";
  import Icon, { type IconGlyph } from "./Icon.svelte";
  import { avatar } from "./avatar.variants";
  import { avatarImageFailed, avatarImageVisible, type AvatarContract } from "./core/avatar";

  interface SharedProps extends AvatarContract, Omit<HTMLAttributes<HTMLSpanElement>, "children" | "class"> {
    /** Decorative content alternative used whenever the image is unavailable. */
    icon?: IconGlyph;
    /** Extra classes merged onto the avatar root. */
    class?: string;
  }

  type Props = SharedProps & (
    | {
        icon: IconGlyph;
        fallback?: string;
      }
    | {
        icon?: undefined;
        fallback: string;
      }
  );

  let {
    src,
    name,
    icon,
    fallback,
    size = "md",
    class: className,
    imageClass,
    fallbackClass,
    ...rest
  }: Props = $props();

  let failedSrc = $state<string>();
  let image = $state<HTMLImageElement>();
  let requestedSrc = untrack(() => src);
  $effect(() => { if (src !== requestedSrc) { requestedSrc = src; failedSrc = undefined; } });
  $effect(() => { if (avatarImageFailed(image)) failedSrc = src; });
  const styles = $derived(avatar({ size }));
</script>

<span
  {...rest}
  data-avatar
  role="img"
  aria-label={name}
  class={styles.root({ class: className })}
>
  <span data-avatar-fallback aria-hidden="true" class={styles.fallback({ class: fallbackClass })}>
    {#if icon !== undefined}
      <Icon {icon} {size} aria-hidden="true" data-avatar-icon />
    {:else}
      {fallback}
    {/if}
  </span>
  {#if avatarImageVisible(src, failedSrc)}
    {#key src}
    <img
      data-avatar-image
      bind:this={image}
      src={src}
      alt=""
      aria-hidden="true"
      style="aspect-ratio: 1 / 1;"
      class={styles.image({ class: imageClass })}
      onerror={() => (failedSrc = src)}
    />
    {/key}
  {/if}
</span>
