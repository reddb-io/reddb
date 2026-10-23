<!-- A Product Application-owned nested appearance scope around the canonical date grid. -->
<script lang="ts">
  import { Calendar, RangeCalendar } from "@reddb-io/design-system/base";
</script>

<div data-theme="nested" data-color-scheme="dark" data-density="compact" data-appearance-scope>
  <Calendar label="Deployment date" class="max-w-sm" data-nested-calendar />
  <RangeCalendar label="Deployment window" class="max-w-sm" data-nested-range-calendar />
</div>
