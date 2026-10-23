<script lang="ts">
  import { Avatar, Carousel, MediaObject } from "@reddb-io/design-system/base";
</script>

{#snippet portrait()}
  <Avatar name="Ada Lovelace" fallback="AL" />
{/snippet}

{#snippet biography()}
  <div>
    <strong>Ada Lovelace</strong>
    <a href="/people/ada">Read biography</a>
  </div>
{/snippet}

{#snippet firstSlide()}
  <button type="button">Open architecture</button>
{/snippet}

{#snippet secondSlide()}
  <p>Delivery</p>
{/snippet}

<div
  data-media-appearance-scope
  data-theme="marketing"
  data-color-scheme="dark"
  data-density="compact"
>
  <MediaObject media={portrait}>
    {@render biography()}
  </MediaObject>

  <Carousel
    label="Platform capabilities"
    slides={[
      { label: "Architecture", content: firstSlide },
      { label: "Delivery", content: secondSlide },
    ]}
  />
</div>
