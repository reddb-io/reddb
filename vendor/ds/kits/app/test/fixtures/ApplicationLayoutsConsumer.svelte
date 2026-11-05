<script lang="ts">
  import {
    ApplicationShell,
    MultiColumnLayout,
    SidebarLayout,
  } from "@reddb-io/design-system/app";
</script>

<div data-theme="base" data-color-scheme="dark" data-density="compact">
  <ApplicationShell aria-label="Deployment application">
    {#snippet header()}<a href="/">Product</a>{/snippet}
    {#snippet children()}
      <SidebarLayout mainId="deployment-content" sidebarLabel="Deployment navigation">
        {#snippet sidebar()}<a href="/deployments">Deployments</a>{/snippet}
        {#snippet children()}
          <MultiColumnLayout>
            {#snippet start()}<button type="button">Filters</button>{/snippet}
            {#snippet children()}<button type="button">Deploy</button>{/snippet}
            {#snippet end()}<button type="button">Inspect</button>{/snippet}
          </MultiColumnLayout>
        {/snippet}
      </SidebarLayout>
    {/snippet}
    {#snippet footer()}Status{/snippet}
  </ApplicationShell>
</div>
