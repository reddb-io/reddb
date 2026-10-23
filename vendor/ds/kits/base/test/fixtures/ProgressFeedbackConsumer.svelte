<script lang="ts">
  import {
    LoadingIndicator,
    Meter,
    Progress,
    RadialProgress,
    Skeleton,
  } from "@reddb-io/design-system/base";
</script>

<section
  data-progress-feedback-scope
  data-theme="marketing"
  data-color-scheme="dark"
  data-density="compact"
>
  <button type="button">Before feedback</button>
  <Progress value={64} label="Deployment progress" />
  <Progress label="Preparing deployment" />
  <RadialProgress value={64} label="Indexing progress" />
  <Meter value={3} max={5} label="Cluster capacity" />
  <LoadingIndicator label="Loading nodes…" />
  <Skeleton label="Loading node summary" />
  <Skeleton shape="circle" />
  <button type="button">After feedback</button>
</section>
