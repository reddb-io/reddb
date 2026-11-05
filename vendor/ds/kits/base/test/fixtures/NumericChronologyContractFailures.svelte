<script lang="ts">
  let { failure }: { failure: "silent-countdown" | "unordered-timeline" } = $props();
</script>

{#if failure === "silent-countdown"}
  <!-- Completion is visible, but no live status delivers the change. -->
  <div data-broken-countdown>
    <span>Deployment window</span>
    <span>Complete</span>
  </div>
{:else}
  <!-- Visual stacking suggests chronology, but the document exposes no order. -->
  <div data-broken-timeline aria-label="Deployment history">
    <div><time datetime="2026-08-08T19:00:00Z">19:00</time> Queued</div>
    <div><time datetime="2026-08-08T19:05:00Z">19:05</time> Deployed</div>
  </div>
{/if}
