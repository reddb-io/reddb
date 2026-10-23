<!-- A segmented calendar value composed from the canonical Field and Input. -->
<script lang="ts">
  import { untrack } from "svelte";
  import { possibleDate, segmentDigits } from "./core/date-segments";
  import { afterFormReset } from "./core/native-controls";
  import type { DateFieldContract } from "./core/contracts/datefield";
  import Field from "./Field.svelte";
  import Input from "./Input.svelte";
  import { dateField } from "./date-field.variants";

  interface Props extends DateFieldContract {
    /** Called with the new value whenever it changes (ADR 0026's value dialect). */
    onvaluechange?: (value: string) => void;
    /** Extra classes merged onto the canonical Field root. */
    class?: string;
  }

  let {
    label,
    name,
    value = $bindable(""),
    onvaluechange,
    help,
    error,
    required = false,
    disabled = false,
    id,
    class: className,
    segmentClass,
  }: Props = $props();


  const initialValue = untrack(() => value);
  const initial = possibleDate(initialValue);
  let year = $state(initial?.[1] ?? "");
  let month = $state(initial?.[2] ?? "");
  let day = $state(initial?.[3] ?? "");
  let yearControl = $state<HTMLInputElement>();
  let monthControl = $state<HTMLInputElement>();
  let dayControl = $state<HTMLInputElement>();
  let publishedValue = value;
  const complete = $derived(possibleDate(`${year}-${month}-${day}`) !== null);
  const styles = dateField();

  $effect(() => {
    if (value === publishedValue) return;

    const next = possibleDate(value);
    year = next?.[1] ?? "";
    month = next?.[2] ?? "";
    day = next?.[3] ?? "";
    publishedValue = value;
  });

  $effect(() => afterFormReset(yearControl?.form, () => {
    const reset = possibleDate(initialValue); year = reset?.[1] ?? ""; month = reset?.[2] ?? ""; day = reset?.[3] ?? "";
    publishedValue = initialValue; value = initialValue;
  }));

  type Segment = "year" | "month" | "day";

  function publish(): void {
    const before = value;
    publishedValue = complete ? `${year}-${month}-${day}` : "";
    value = publishedValue;
    if (value !== before) onvaluechange?.(value);
  }

  function handleInput(event: Event, segment: Segment): void {
    const input = event.currentTarget as HTMLInputElement;
    const length = segment === "year" ? 4 : 2;
    const next = segmentDigits(input.value, segment);

    input.value = next;
    if (segment === "year") year = next;
    else if (segment === "month") month = next;
    else day = next;
    publish();

    if (next.length === length) {
      (segment === "year" ? monthControl : segment === "month" ? dayControl : undefined)?.focus();
    }
  }

  function handleKeydown(event: KeyboardEvent, segment: Segment): void {
    const previous = segment === "day" ? monthControl : segment === "month" ? yearControl : undefined;
    const next = segment === "year" ? monthControl : segment === "month" ? dayControl : undefined;

    if (event.key === "ArrowLeft" && previous) {
      event.preventDefault();
      previous.focus();
    } else if (event.key === "ArrowRight" && next) {
      event.preventDefault();
      next.focus();
    }
  }
</script>

<Field {label} {help} {error} {required} {id} class={styles.root({ class: className })}>
  {#snippet children(control)}
    <div class={styles.segments()} data-date-field data-date-segments>
      <Input
        {...control}
        bind:ref={yearControl}
        type="text"
        inputmode="numeric"
        maxlength={4}
        value={year}
        {disabled}
        aria-label={`${label}, year`}
        data-date-segment="year"
        class={styles.year({ class: segmentClass })}
        oninput={(event) => handleInput(event, "year")}
        onkeydown={(event) => handleKeydown(event, "year")}
      />
      <span class={styles.separator()} aria-hidden="true">-</span>
      <Input
        bind:ref={monthControl}
        id={`${control.id}-month`}
        type="text"
        inputmode="numeric"
        maxlength={2}
        value={month}
        required={control.required}
        {disabled}
        aria-label={`${label}, month`}
        aria-describedby={control["aria-describedby"]}
        aria-invalid={control["aria-invalid"]}
        aria-errormessage={control["aria-errormessage"]}
        data-date-segment="month"
        class={styles.segment({ class: segmentClass })}
        oninput={(event) => handleInput(event, "month")}
        onkeydown={(event) => handleKeydown(event, "month")}
      />
      <span class={styles.separator()} aria-hidden="true">-</span>
      <Input
        bind:ref={dayControl}
        id={`${control.id}-day`}
        type="text"
        inputmode="numeric"
        maxlength={2}
        value={day}
        required={control.required}
        {disabled}
        aria-label={`${label}, day`}
        aria-describedby={control["aria-describedby"]}
        aria-invalid={control["aria-invalid"]}
        aria-errormessage={control["aria-errormessage"]}
        data-date-segment="day"
        class={styles.segment({ class: segmentClass })}
        oninput={(event) => handleInput(event, "day")}
        onkeydown={(event) => handleKeydown(event, "day")}
      />
      {#if name && complete && !disabled}<input type="hidden" {name} {value} />{/if}
    </div>
  {/snippet}
</Field>
