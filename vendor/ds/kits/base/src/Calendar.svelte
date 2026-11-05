<!-- A single-date calendar composing Bits UI behavior with canonical Base Button controls. -->
<script lang="ts">
  import { calendarDirection, calendarKeyTarget, calendarLocale, type CalendarContract } from "./core/calendar";
  import { tick } from "svelte";
  import type { DateValue } from "@internationalized/date";
  import { Calendar as Bits } from "bits-ui";
  import Button from "./Button.svelte";
  import { calendar } from "./calendar.variants";
  import { quietControl } from "./quiet-control.variants";

  type RootProps = Extract<Bits.RootProps, { type: "single" }>;
  type Props = CalendarContract & Omit<
    RootProps,
    | "calendarLabel"
    | "child"
    | "children"
    | "class"
    | "onPlaceholderChange"
    | "onValueChange"
    | "placeholder"
    | "type"
    | "value"
  > & {
    /** Focus the selected date, today or first enabled day on mount. */
    initialFocus?: boolean;
    /** Base accessible name; the visible month and year are appended by the calendar. */
    label: string;
    /** The selected date, bindable. Leave it unset for no selection. */
    value?: DateValue;
    /**
     * A date inside the month shown while nothing is selected; it moves as the user navigates.
     * Bindable.
     */
    placeholder?: DateValue;
    /** Called with the newly selected date, or `undefined` when the selection is cleared. */
    onvaluechange?: (value: DateValue | undefined) => void;
    /** Called with a date inside the visible month each time the user moves to another month. */
    onplaceholderchange?: (value: DateValue) => void;
    /** Extra classes merged onto the calendar root. */
    class?: string;
    /** Extra classes merged onto the previous-month and next-month Buttons. */
    navigationClass?: string;
    /** Extra classes merged onto each day button. */
    dayClass?: string;
  };

  let {
    label,
    value = $bindable(),
    placeholder = $bindable(),
    weekdayFormat = "short",
    fixedWeeks = true,
    preventDeselect = false,
    onvaluechange,
    onplaceholderchange,
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
  type="single"
  dir={rest.dir ?? calendarDirection(rest.locale ?? "en")}
  locale={calendarLocale(rest.locale ?? "en", placeholder ?? value)}
  calendarLabel={label}
  {weekdayFormat}
  {fixedWeeks}
  {preventDeselect}
  bind:value
  bind:placeholder
  onValueChange={onvaluechange}
  onPlaceholderChange={onplaceholderchange}
  data-calendar
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
