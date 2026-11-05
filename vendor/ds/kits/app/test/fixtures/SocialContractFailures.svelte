<script lang="ts">
  interface Props {
    failure: "unordered-feed" | "colour-only-rating";
  }

  let { failure }: Props = $props();
</script>

{#if failure === "unordered-feed"}
  <div data-broken-activity-feed aria-label="Deployment activity">
    <div><time datetime="2026-08-08T19:00:00Z">19:00</time> Queued</div>
    <div><time datetime="2026-08-08T19:05:00Z">19:05</time> Deployed</div>
  </div>
{:else}
  <article data-broken-review aria-label="Review by Ada Lovelace">
    <p>Ada Lovelace</p>
    <p aria-label="Rating"><span class="text-primary">★★★★★</span></p>
    <p>Clear and dependable.</p>
  </article>
{/if}
