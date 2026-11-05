<!-- A segmented clock value composed from the canonical Field and Input. -->
<script lang="ts">
  import { untrack } from "svelte";
  import { possibleTime, segmentDigits } from "./core/date-segments";
  import { afterFormReset } from "./core/native-controls";
  import type { TimeFieldContract } from "./core/contracts/timefield";
  import Field from "./Field.svelte";
  import Input from "./Input.svelte";
  import { timeField } from "./time-field.variants";

  interface Props extends TimeFieldContract {
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
  const initial = possibleTime(initialValue);
  let publishedValue = initialValue;
  let hour = $state(initial?.[1] ?? "");
  let minute = $state(initial?.[2] ?? "");
  let hourControl = $state<HTMLInputElement>();
  let minuteControl = $state<HTMLInputElement>();
  const complete = $derived(possibleTime(`${hour}:${minute}`) !== null);
  const styles = timeField();

  $effect(() => {
    if (value === publishedValue) return;
    const next = possibleTime(value); hour = next?.[1] ?? ""; minute = next?.[2] ?? ""; publishedValue = value;
  });
  $effect(() => afterFormReset(hourControl?.form, () => {
    const next = possibleTime(initialValue); hour = next?.[1] ?? ""; minute = next?.[2] ?? ""; publishedValue = initialValue; value = initialValue;
  }));

  function publish(): void {
    const before = value;
    publishedValue = complete ? `${hour}:${minute}` : "";
    value = publishedValue;
    if (value !== before) onvaluechange?.(value);
  }

  function handleInput(event: Event, segment: "hour" | "minute"): void {
    const input = event.currentTarget as HTMLInputElement;
    const next = segmentDigits(input.value, segment);

    input.value = next;
    if (segment === "hour") hour = next;
    else minute = next;
    publish();
    if (next.length === 2) (segment === "hour" ? minuteControl : undefined)?.focus();
  }

  function handleKeydown(event: KeyboardEvent, segment: "hour" | "minute"): void {
    if (event.key === "ArrowLeft" && segment === "minute") {
      event.preventDefault();
      hourControl?.focus();
    } else if (event.key === "ArrowRight" && segment === "hour") {
      event.preventDefault();
      minuteControl?.focus();
    }
  }
</script>

<Field {label} {help} {error} {required} {id} class={styles.root({ class: className })}>
  {#snippet children(control)}
    <div class={styles.segments()} data-time-field data-time-segments>
      <Input
        {...control}
        bind:ref={hourControl}
        type="text"
        inputmode="numeric"
        maxlength={2}
        value={hour}
        {disabled}
        aria-label={`${label}, hour`}
        data-time-segment="hour"
        class={styles.segment({ class: segmentClass })}
        oninput={(event) => handleInput(event, "hour")}
        onkeydown={(event) => handleKeydown(event, "hour")}
      />
      <span class={styles.separator()} aria-hidden="true">:</span>
      <Input
        bind:ref={minuteControl}
        id={`${control.id}-minute`}
        type="text"
        inputmode="numeric"
        maxlength={2}
        value={minute}
        required={control.required}
        {disabled}
        aria-label={`${label}, minute`}
        aria-describedby={control["aria-describedby"]}
        aria-invalid={control["aria-invalid"]}
        aria-errormessage={control["aria-errormessage"]}
        data-time-segment="minute"
        class={styles.segment({ class: segmentClass })}
        oninput={(event) => handleInput(event, "minute")}
        onkeydown={(event) => handleKeydown(event, "minute")}
      />
      {#if name && complete && !disabled}<input type="hidden" {name} {value} />{/if}
    </div>
  {/snippet}
</Field>
