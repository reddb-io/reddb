<script lang="ts">
  import { Reveal } from "@reddb-io/design-system/base";

  const themes = ["application", "marketing"];
  const schemes = ["light", "dark"];
  const densities = ["tiny", "compact", "comfortable", "spacious"];
</script>

{#each themes as theme}
  {#each schemes as scheme}
    {#each densities as density}
      <div data-appearance-scope data-theme={theme} data-color-scheme={scheme} data-density={density}>
        <Reveal>Visible in every appearance</Reveal>
      </div>
    {/each}
  {/each}
{/each}
