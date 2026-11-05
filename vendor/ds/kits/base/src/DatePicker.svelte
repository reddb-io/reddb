<!-- A synchronized DateField and Calendar under the canonical Popover lifecycle. -->
<script lang="ts">
  import type { DatePickerContract } from "./core/contracts/datepicker";
  import { parseCalendarDate as toDateValue } from "./core/calendar";
  import type { DateValue } from "@internationalized/date";
  import type { Snippet } from "svelte";
  import Calendar from "./Calendar.svelte";
  import DateField from "./DateField.svelte";
  import Popover from "./Popover.svelte";
  import { datePicker } from "./date-picker.variants";

  type Side = "top" | "right" | "bottom" | "left";
  type Align = "start" | "center" | "end";

  interface Props extends DatePickerContract {
    /** Optional caller-owned content inside the canonical trigger Button. */
    trigger?: Snippet;
    /** Extra classes merged onto the picker root, which lays the field and trigger out in a row. */
    class?: string;
    /** Called with the complete ISO date after the user picks one in the Calendar. */
    onvaluechange?: (value: string) => void;
    /** Called with the next open state when the calendar Popover opens or closes. */
    onopenchange?: (open: boolean) => void;
  }

  let {
    label,
    calendarLabel,
    triggerLabel = `Choose ${label}`,
    name,
    value = $bindable(""),
    help,
    error,
    required = false,
    disabled = false,
    id,
    open = $bindable(false),
    placeholder,
    minValue,
    maxValue,
    isDateUnavailable,
    side,
    align,
    sideOffset,
    collisionPadding,
    trigger,
    class: className,
    fieldClass,
    segmentClass,
    triggerClass,
    contentClass,
    calendarClass,
    navigationClass,
    dayClass,
    onvaluechange,
    onopenchange,
  }: Props = $props();

  const styles = datePicker();
  const calendarValue = $derived(toDateValue(value));


  function handleCalendarValue(next: DateValue | undefined): void {
    if (!next) return;
    value = next.toString();
    onvaluechange?.(value);
    open = false;
  }
</script>

<div class={styles.root({ class: className })} data-date-picker>
  <DateField
    {label}
    {name}
    bind:value
    {help}
    {error}
    {required}
    {disabled}
    {id}
    class={styles.field({ class: fieldClass })}
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
    class={styles.content({ class: contentClass })}
    onopenchange={onopenchange}
  >
    <Calendar
      label={calendarLabel}
      value={calendarValue}
      placeholder={placeholder ?? calendarValue}
      {minValue}
      {maxValue}
      {isDateUnavailable}
      disabled={disabled}
      onvaluechange={handleCalendarValue}
      class={styles.calendar({ class: calendarClass })}
      {navigationClass}
      {dayClass}
    />
  </Popover>
</div>
