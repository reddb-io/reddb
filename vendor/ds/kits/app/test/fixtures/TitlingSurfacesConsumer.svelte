<script lang="ts">
  import { ActionPanel, CardHeading, PageHeading } from "@reddb-io/design-system/app";
</script>

<div data-theme="base" data-color-scheme="dark" data-density="compact">
  <PageHeading title="Deployments" description="Manage releases.">
    {#snippet actions()}<button type="button">New deployment</button>{/snippet}
  </PageHeading>

  <CardHeading title="Environment" level={3}>
    {#snippet actions()}<button type="button">Edit</button>{/snippet}
  </CardHeading>

  <ActionPanel title="Production deployment">
    <p>Review the configuration before saving.</p>
    {#snippet actions()}
      <button type="button">Cancel</button>
      <button type="button">Save</button>
    {/snippet}
  </ActionPanel>
</div>
