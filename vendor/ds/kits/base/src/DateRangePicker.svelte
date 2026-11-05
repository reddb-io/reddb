<!-- Synchronized DateRangeField and RangeCalendar under the canonical Popover lifecycle. -->
<script lang="ts">
  import type { DateRangePickerContract } from "./core/contracts/daterangepicker";
  import { parseCalendarDate as toDateValue } from "./core/calendar";
  import type { DateValue } from "@internationalized/date";
  import type { Snippet } from "svelte";
  import DateRangeField from "./DateRangeField.svelte";
  import Popover from "./Popover.svelte";
  import RangeCalendar from "./RangeCalendar.svelte";
  import { dateRangePicker } from "./date-range-picker.variants";

  type DateRange = { start: DateValue | undefined; end: DateValue | undefined };
  type StringDateRange = { start: string; end: string };
  type Side = "top" | "right" | "bottom" | "left";
  type Align = "start" | "center" | "end";

  interface Props extends DateRangePickerContract {
    /** Custom content inside the trigger Button. The trigger label text is shown when omitted. */
    trigger?: Snippet;
    /** Extra classes merged onto the picker root, which lays the field and trigger out in a row. */
    class?: string;
    /**
     * Called with `{ start, end }` ISO dates once the user has chosen a complete range in the
     * calendar.
     */
    onvaluechange?: (value: StringDateRange) => void;
    /** Called with the next open state when the calendar Popover opens or closes. */
    onopenchange?: (open: boolean) => void;
  }

  let {
    label,
    startLabel,
    endLabel,
    calendarLabel,
    triggerLabel = `Choose ${label}`,
    startName,
    endName,
    startValue = $bindable(""),
    endValue = $bindable(""),
    help,
    rangeError,
    required = false,
    disabled = false,
    id,
    open = $bindable(false),
    placeholder,
    minValue,
    maxValue,
    minDays,
    maxDays,
    isDateUnavailable,
    side,
    align,
    sideOffset,
    collisionPadding,
    trigger,
    class: className,
    fieldClass,
    dateFieldClass,
    segmentClass,
    triggerClass,
    contentClass,
    calendarClass,
    navigationClass,
    dayClass,
    onvaluechange,
    onopenchange,
  }: Props = $props();

  const styles = dateRangePicker();
  const calendarValue = $derived<DateRange>({
    start: toDateValue(startValue),
    end: toDateValue(endValue),
  });


  function handleCalendarValue(next: DateRange): void {
    startValue = next.start?.toString() ?? "";
    endValue = next.end?.toString() ?? "";

    if (startValue && endValue) {
      onvaluechange?.({ start: startValue, end: endValue });
      open = false;
    }
  }
</script>

<div class={styles.root({ class: className })} data-date-range-picker>
  <DateRangeField
    {label}
    {startLabel}
    {endLabel}
    {startName}
    {endName}
    bind:startValue
    bind:endValue
    {help}
    {rangeError}
    {required}
    {disabled}
    {id}
    class={styles.field({ class: fieldClass })}
    fieldClass={dateFieldClass}
    {segmentClass}
  />
  <Popover
    {triggerLabel}
    contentLabel={calendarLabel}
    bind:open
    {disabled}
    {side}
    {align}
    {sideOffset}
    {collisionPadding}
    {trigger}
    triggerClass={styles.trigger({ class: triggerClass })}
    class={contentClass}
    onopenchange={onopenchange}
  >
    <RangeCalendar
      label={calendarLabel}
      value={calendarValue}
      placeholder={placeholder ?? calendarValue.start}
      {minValue}
      {maxValue}
      {minDays}
      {maxDays}
      {isDateUnavailable}
      {disabled}
      onvaluechange={handleCalendarValue}
      class={styles.calendar({ class: calendarClass })}
      {navigationClass}
      {dayClass}
    />
  </Popover>
</div>
