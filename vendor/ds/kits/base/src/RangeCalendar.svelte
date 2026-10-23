<!-- A date-range calendar sharing Calendar appearance while Bits UI owns range behavior. -->
<script lang="ts">
  import { calendarDirection, calendarKeyTarget, calendarLocale, type CalendarContract } from "./core/calendar";
  import { tick } from "svelte";
  import type { DateValue } from "@internationalized/date";
  import { RangeCalendar as Bits } from "bits-ui";
  import Button from "./Button.svelte";
  import { calendar } from "./calendar.variants";
  import { quietControl } from "./quiet-control.variants";

  import type { CalendarRange as DateRange } from "./core/calendar";
  type Props = CalendarContract & Omit<
    Bits.RootProps,
    | "calendarLabel"
    | "child"
    | "children"
    | "class"
    | "onEndValueChange"
    | "onPlaceholderChange"
    | "onStartValueChange"
    | "onValueChange"
    | "placeholder"
    | "value"
  > & {
    /** Fewest calendar days in a complete inclusive range. */
    minDays?: number;
    /** Most calendar days in a complete inclusive range. */
    maxDays?: number;
    /** Restart a selection that would contain disabled dates. */
    excludeDisabled?: boolean;
    /** Base accessible name; the visible month and year are appended by the calendar. */
    label: string;
    /** The selected date range, with `start` and `end` dates; bindable. */
    value?: DateRange;
    /** The date that decides which month is shown while no range is selected; bindable. */
    placeholder?: DateValue;
    /** Called with the new date range after the selection changes. */
    onvaluechange?: (value: DateRange) => void;
    /** Called with the new placeholder date after the visible month changes. */
    onplaceholderchange?: (value: DateValue) => void;
    /**
     * Called with the new start date, or `undefined` when it is cleared, after the start of the
     * range changes.
     */
    onstartvaluechange?: (value: DateValue | undefined) => void;
    /**
     * Called with the new end date, or `undefined` when it is cleared, after the end of the range
     * changes.
     */
    onendvaluechange?: (value: DateValue | undefined) => void;
    /** Extra classes, merged over the root slot's own. */
    class?: string;
    /** Extra classes merged onto both the previous-month and next-month buttons. */
    navigationClass?: string;
    /** Extra classes merged onto every day button, over its own. */
    dayClass?: string;
  };

  let {
    label,
    value = $bindable(),
    placeholder = $bindable(),
    weekdayFormat = "short",
    fixedWeeks = true,
    excludeDisabled = false,
    onvaluechange,
    onplaceholderchange,
    onstartvaluechange,
    onendvaluechange,
    class: className,
    navigationClass,
    dayClass,
    ...rest
  }: Props = $props();

  const styles = calendar();
  function weekEdge(event: KeyboardEvent, date: DateValue): void {
    if (event.key !== "Home" && event.key !== "End") return;
    const next = calendarKeyTarget(date, event.key, { label, ...rest });
    if (!next) return;
    event.preventDefault();
    const root = (event.currentTarget as HTMLElement).closest("[data-calendar], [data-range-calendar]");
    const focus = () => root?.querySelector<HTMLElement>(`[data-bits-day][data-value="${next.toString()}"]`)?.focus();
    if (root?.querySelector(`[data-bits-day][data-value="${next.toString()}"]`)) focus();
    else { placeholder = next; void tick().then(focus); }
  }
</script>

<Bits.Root
  {...rest}
  dir={rest.dir ?? calendarDirection(rest.locale ?? "en")}
  locale={calendarLocale(rest.locale ?? "en", placeholder ?? value?.start)}
  calendarLabel={label}
  {weekdayFormat}
  {fixedWeeks}
  {excludeDisabled}
  bind:value
  bind:placeholder
  onValueChange={onvaluechange}
  onPlaceholderChange={onplaceholderchange}
  onStartValueChange={onstartvaluechange}
  onEndValueChange={onendvaluechange}
  data-range-calendar
  class={styles.root({ class: className })}
>
  {#snippet children({ months, weekdays })}
    <!--
      A <div>, not the <header> Bits renders by default: a header outside
      any sectioning element is a banner landmark, and a Calendar inside a
      DatePicker popover became a second banner on the page (wave 5A).
    -->
    <Bits.Header class={styles.header()}>
      {#snippet child({ props })}
        <div {...props}>
          <Bits.PrevButton>
            {#snippet child({ props })}
              <Button
                {...props}
                variant="ghost"
                size="sm"
                aria-label="Previous month"
                class={styles.navigation({ class: navigationClass })}
              >
                <span aria-hidden="true">←</span>
              </Button>
            {/snippet}
          </Bits.PrevButton>
          <Bits.Heading class={styles.heading()} />
          <Bits.NextButton>
            {#snippet child({ props })}
              <Button
                {...props}
                variant="ghost"
                size="sm"
                aria-label="Next month"
                class={styles.navigation({ class: navigationClass })}
              >
                <span aria-hidden="true">→</span>
              </Button>
            {/snippet}
          </Bits.NextButton>
        </div>
      {/snippet}
    </Bits.Header>

    <div class={styles.months()}>
      {#each months as month}
        <Bits.Grid class={styles.grid()}>
          <Bits.GridHead class={styles.gridHead()}>
            <Bits.GridRow class={styles.gridRow()}>
              {#each weekdays as weekday, index (index)}
                <Bits.HeadCell class={styles.headCell()}>{weekday}</Bits.HeadCell>
              {/each}
            </Bits.GridRow>
          </Bits.GridHead>
          <Bits.GridBody>
            {#each month.weeks as weekDates, weekIndex (weekIndex)}
              <Bits.GridRow class={styles.gridRow()}>
                {#each weekDates as date (date.toString())}
                  <Bits.Cell {date} month={month.value} class={styles.cell()}>
                    <Bits.Day onkeydown={(event) => weekEdge(event, date)} class={quietControl({ ink: "inherit", class: styles.day({ class: dayClass }) })} />
                  </Bits.Cell>
                {/each}
              </Bits.GridRow>
            {/each}
          </Bits.GridBody>
        </Bits.Grid>
      {/each}
    </div>
  {/snippet}
</Bits.Root>
