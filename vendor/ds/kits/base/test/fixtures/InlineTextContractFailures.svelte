<!-- Deliberately broken examples: tests pin the regressions these primitives prevent. -->
<script lang="ts">
  interface Props {
    failure: "colour-only-link" | "collapsed-code";
    source?: string;
  }

  const { failure, source = "const answer = 42;" }: Props = $props();
  const collapsed = $derived(source.trim().replace(/\s+/g, " "));
</script>

{#if failure === "colour-only-link"}
  <p>
    Read the
    <a data-broken-link class="text-primary" href="/guide">usage guide</a>
    before continuing.
  </p>
{:else}
  <code data-broken-code>{collapsed}</code>
{/if}
