<script lang="ts">
  import { Diff, type DiffRow } from "@reddb-io/design-system/app";

  const rows: readonly DiffRow[] = [
    { before: "region = us-east-1", after: "region = us-east-1" },
    { before: "replicas = 2", after: "replicas = 3" },
  ];
</script>

<section
  data-diff-consumer
  data-theme="application"
  data-color-scheme="dark"
  data-density="compact"
  data-motion="reduced"
>
  <Diff label="Deployment changes" {rows} mode="side-by-side" />
</section>
