<script lang="ts">
  interface Props {
    failure: "swallowed-content" | "interactive-chrome";
  }

  const { failure }: Props = $props();
</script>

{#if failure === "swallowed-content"}
  <div data-broken-mockup aria-hidden="true">
    <article aria-label="Deployment preview">
      <button type="button">Inspect deployment</button>
    </article>
  </div>
{:else}
  <div data-broken-browser-mockup>
    <div data-broken-browser-chrome>
      <a href="/reload">Reload</a>
      <input aria-label="Address" value="https://example.test/deployments" />
    </div>
    <article aria-label="Deployment preview">Caller-owned content</article>
  </div>
{/if}
