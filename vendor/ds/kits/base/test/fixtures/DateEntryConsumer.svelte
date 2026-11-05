<!-- Date entry contracts composed only through the distributed Base subpath,
     including a nested appearance scope owned by the consumer. -->
<script lang="ts">
  import { DateField, DateRangeField } from "@reddb-io/design-system/base";
</script>

<form>
  <DateField
    label="Deployment date"
    name="deploymentDate"
    value="2026-08-20"
    help="Use the release calendar."
    required
  />

  <DateRangeField
    label="Deployment window"
    startLabel="Starts"
    endLabel="Ends"
    startName="deploymentStart"
    endName="deploymentEnd"
    startValue="2026-08-10"
    endValue="2026-08-20"
  />

  <div
    data-appearance-scope
    data-theme="marketing"
    data-color-scheme="dark"
    data-density="compact"
  >
    <div data-nested-date-field>
      <DateField label="Nested deployment date" value="2026-09-01" />
    </div>
    <div data-nested-date-range-field>
      <DateRangeField
        label="Nested deployment window"
        startLabel="Starts"
        endLabel="Ends"
        startValue="2026-09-01"
        endValue="2026-09-05"
      />
    </div>
  </div>
</form>
