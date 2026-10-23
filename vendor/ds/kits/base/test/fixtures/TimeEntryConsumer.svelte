<!-- Time entry contracts composed only through the distributed Base subpath,
     including a nested appearance scope owned by the consumer. -->
<script lang="ts">
  import { TimeField, TimeRangeField } from "@reddb-io/design-system/base";
</script>

<form>
  <TimeField
    label="Meeting time"
    name="meetingTime"
    value="09:45"
    help="Use local time."
    required
  />

  <TimeRangeField
    label="Support window"
    startLabel="Starts"
    endLabel="Ends"
    startName="supportStart"
    endName="supportEnd"
    startValue="09:00"
    endValue="17:00"
  />

  <div
    data-appearance-scope
    data-theme="marketing"
    data-color-scheme="dark"
    data-density="compact"
  >
    <div data-nested-time-field>
      <TimeField label="Nested meeting time" value="10:15" />
    </div>
    <div data-nested-time-range-field>
      <TimeRangeField
        label="Nested support window"
        startLabel="Starts"
        endLabel="Ends"
        startValue="10:00"
        endValue="18:00"
      />
    </div>
  </div>
</form>
