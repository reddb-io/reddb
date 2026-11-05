<!-- Picker contracts composed only through the distributed Base subpath,
     nested under every consumer-owned appearance axis. -->
<script lang="ts">
  import { DatePicker, DateRangePicker } from "@reddb-io/design-system/base";
</script>

<form
  data-appearance-scope
  data-theme="marketing"
  data-color-scheme="dark"
  data-density="compact"
>
  <div data-nested-date-picker>
    <DatePicker
      label="Deployment date"
      calendarLabel="Deployment date calendar"
      name="deploymentDate"
      value="2026-06-10"
    />
  </div>

  <div data-nested-date-range-picker>
    <DateRangePicker
      label="Deployment window"
      startLabel="Starts"
      endLabel="Ends"
      calendarLabel="Deployment window calendar"
      startName="deploymentStart"
      endName="deploymentEnd"
      startValue="2026-06-10"
      endValue="2026-06-12"
    />
  </div>
</form>
