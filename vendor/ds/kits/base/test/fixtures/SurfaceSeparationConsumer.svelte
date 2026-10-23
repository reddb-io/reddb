<script lang="ts">
  import { AspectRatio, Card, Divider } from "@reddb-io/design-system/base";
</script>

<div
  data-surface-appearance-scope
  data-theme="marketing"
  data-color-scheme="dark"
  data-density="compact"
>
  <Card title="Deployment" description="Caller-owned surface content.">
    <AspectRatio ratio={4 / 3} aria-label="Deployment preview">
      <button type="button">Open preview</button>
    </AspectRatio>
    <Divider />
    <p>Healthy in sa-east-1.</p>
  </Card>

  <Divider orientation="vertical" aria-label="Column boundary" />
</div>
