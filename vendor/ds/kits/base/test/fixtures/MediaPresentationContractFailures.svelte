<script lang="ts">
  interface Props {
    failure: "avatar-without-fallback" | "unpausable-carousel";
  }

  const { failure }: Props = $props();
</script>

{#if failure === "avatar-without-fallback"}
  <span data-broken-avatar role="img" aria-label="Ada Lovelace">
    <img src="/missing-ada.png" alt="" />
  </span>
{:else}
  <section
    data-broken-carousel
    data-auto-rotates
    role="region"
    aria-roledescription="carousel"
    aria-label="Featured work"
  >
    <p>Rotation starts automatically, with no control that can pause it.</p>
  </section>
{/if}
