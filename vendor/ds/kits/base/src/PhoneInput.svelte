<script lang="ts">
  import { onMount, untrack } from "svelte";
  import type { HTMLInputAttributes } from "svelte/elements";
  import Field from "./Field.svelte";
  import SearchSelect from "./SearchSelect.svelte";
  import { input as inputStyle } from "./input.variants";
  import { phoneInput } from "./phone-input.variants";
  import type { PhoneInputContract, PhoneCountry } from "./core/contracts/phoneinput";
  import { bindPhoneEditing, phoneCountries, phoneCountryName, phoneFlag, phoneInvalid, phoneOptions, phoneProblem, phoneState, type PhoneState } from "./core/phone-input";
  import { afterFormReset, associatedForm } from "./core/native-controls";

  interface Props extends PhoneInputContract {
    /** Additional classes on the field root. */
    class?: string;
    /** Reports unmasked international values after user edits and native form reset. */
    onvaluechange?: (value: string) => void;
    /** Reports country changes from selection or an international paste. */
    oncountrychange?: (country: PhoneCountry) => void;
  }
  let { label, value = $bindable(), defaultValue = "", defaultCountry = "US", countries, locale = "en",
    name, form, id, help, error, required = false, disabled = false, presentation = "input", sourceLabel,
    emptyLabel = "No phone number", placeholder, autoComplete = "tel-national", countryLabel = "Country",
    searchPlaceholder = "Search countries", emptyText = "No matching countries", requiredMessage = "Enter a phone number.",
    invalidMessage = "Enter a complete phone number for an allowed country.", controlSize = "md", class: className,
    onvaluechange, oncountrychange }: Props = $props();
  const initial = untrack(() => phoneState(value ?? defaultValue, phoneCountries(countries).includes(defaultCountry) ? defaultCountry : phoneCountries(countries)[0]!));
  let localized = $state(false);
  onMount(() => { localized = true; });
  let local = $state(initial.value), draft = $state<PhoneState>(initial), invalid = $state(false);
  let element = $state<HTMLInputElement>(), hidden = $state<HTMLInputElement>();
  let editing: ReturnType<typeof bindPhoneEditing> | undefined;
  const allowed = $derived(phoneCountries(countries));
  const currentValue = $derived(value ?? local);
  const current = $derived(currentValue === draft.value ? draft : phoneState(currentValue, draft.country));
  const options = $derived(phoneOptions(allowed, current.country, locale, localized));
  const problem = $derived(phoneProblem(current, allowed, required, requiredMessage, invalidMessage));
  const fieldError = $derived(error || (invalid ? problem : undefined));
  const slots = phoneInput();
  const uid = $props.id();
  function change(next: PhoneState, previous = current) {
    draft = next;
    if (next.value !== previous.value) { local = next.value; value = next.value; onvaluechange?.(next.value); }
    if (next.country !== previous.country) oncountrychange?.(next.country);
  }
  function countryChange(country: string) {
    if (!allowed.includes(country as PhoneCountry) || element?.matches(":disabled")) return;
    change(phoneState(current.display, country as PhoneCountry));
    element?.focus();
  }
  $effect(() => {
    const node = element;
    if (!node) return;
    const binding = bindPhoneEditing(node, untrack(() => current), change); editing = binding;
    return () => { binding.destroy(); if (editing === binding) editing = undefined; };
  });
  $effect(() => { editing?.sync(current); element?.setCustomValidity(problem); });
  $effect(() => {
    const node = hidden;
    if (!node) return;
    return afterFormReset(associatedForm(node, form), () => {
      const previous = current;
      draft = { ...initial }; local = initial.value; value = initial.value; invalid = false;
      onvaluechange?.(initial.value);
      if (previous.country !== initial.country) oncountrychange?.(initial.country);
    });
  });
</script>

<div data-phone-input data-presentation={presentation} class={slots.root({ class: className })}>
  <input bind:this={hidden} type="hidden" {name} {form} value={current.value} {disabled} />
  {#if presentation === "text"}
    <div role="group" aria-labelledby={`${uid}-label`} class={slots.read()}>
      <span id={`${uid}-label`} class={slots.label()}>{label}</span>
      <span data-phone-value class={slots.value()}>
        {#if current.value}<span aria-hidden="true" class={slots.flag()}>{phoneFlag(current.country)}</span>{/if}
        <span class={slots.number()}>{current.international || emptyLabel}</span>
      </span>
      {#if sourceLabel}<span class={slots.help()}>{sourceLabel}</span>{/if}
      {#if help}<span class={slots.help()}>{help}</span>{/if}
      {#if error}<span role="alert" class={slots.error()}>{error}</span>{/if}
    </div>
  {:else}
    <Field {label} {help} error={fieldError} {required} {id}>
      {#snippet children(control)}
        <div class={slots.row()}>
          <SearchSelect label={`${countryLabel}: ${localized ? phoneCountryName(current.country, locale) : current.country}`} {options} value={current.country}
            onvaluechange={countryChange} clearable={false} {disabled} {controlSize} {searchPlaceholder} {emptyText}
            class={slots.country()} contentClass={slots.popup()} />
          <input {...control} bind:this={element} type="tel" inputmode="tel" autocomplete={autoComplete as HTMLInputAttributes["autocomplete"]} {form} {disabled}
            {placeholder} value={initial.display} class={inputStyle({ size: controlSize, class: slots.input() })}
            onblur={() => { invalid = true; }} oninvalid={event => { event.preventDefault(); invalid = true; phoneInvalid(event.currentTarget); }} />
        </div>
      {/snippet}
    </Field>
  {/if}
</div>
