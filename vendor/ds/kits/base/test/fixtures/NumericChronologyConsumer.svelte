<script lang="ts">
  import { Countdown, Statistic, Timeline } from "@reddb-io/design-system/base";
</script>

<section
  data-numeric-chronology-scope
  data-theme="marketing"
  data-color-scheme="dark"
  data-contrast="high"
  data-density="compact"
>
  <button type="button">Before chronology</button>
  <Statistic label="Queries" value={1240} description="Last 24 hours" />
  <Countdown label="Deployment window" remaining={65} />
  <Timeline
    label="Deployment history"
    items={[
      {
        id: "queued",
        title: "Queued",
        datetime: "2026-08-08T19:00:00Z",
        time: "19:00",
        href: "/deployments/queued",
      },
      {
        id: "deployed",
        title: "Deployed",
        datetime: "2026-08-08T19:05:00Z",
        time: "19:05",
      },
    ]}
  />
  <button type="button">After chronology</button>
</section>
