<!-- A Field-bound typeahead whose input retains focus while Bits UI owns listbox semantics. -->
<script lang="ts">
  import { portalAppearance } from "./portal-appearance.svelte";
  import type { ComboboxContract } from "./core/contracts/combobox";
  import { Combobox as Bits } from "bits-ui";
  import Field from "./Field.svelte";
  import { input } from "./input.variants";
  import { combobox, type ComboboxOption, type ComboboxSize } from "./combobox.variants";
  import { popover } from "./popover.variants";

  interface Props extends ComboboxContract {
    /** Extra classes merged onto the Field root that wraps the label, input and messages. */
    class?: string;
    /** Called with the option's `value` when the user selects an option. */
    onvaluechange?: (value: string) => void;
    /**
     * Called with the input text as the user types, and with the option's label after a selection.
     */
    oninputvaluechange?: (value: string) => void;
    /** Called with the next open state when the listbox opens or closes. */
    onopenchange?: (open: boolean) => void;
  }

  let {
    label,
    options,
    name,
    value = $bindable(""),
    inputValue = $bindable(options.find((option) => option.value === value)?.label ?? ""),
    help,
    error,
    required = false,
    disabled = false,
    placeholder,
    open = $bindable(false),
    controlSize = "md",
    class: className,
    inputClass,
    contentClass,
    itemClass,
    onvaluechange,
    oninputvaluechange,
    onopenchange,
  }: Props = $props();

  const styles = $derived(combobox({ size: controlSize }));
  const normalizedInput = $derived(inputValue.trim().toLocaleLowerCase());
  const filteredOptions = $derived(
    normalizedInput === ""
      ? options
      : options.filter((option) => option.label.toLocaleLowerCase().includes(normalizedInput)),
  );

  function handleInput(event: Event): void {
    inputValue = (event.currentTarget as HTMLInputElement).value;
    oninputvaluechange?.(inputValue);
  }

  function handleValueChange(nextValue: string): void {
    value = nextValue;
    inputValue = options.find((option) => option.value === nextValue)?.label ?? "";
    onvaluechange?.(nextValue);
    oninputvaluechange?.(inputValue);
  }
  const portal = portalAppearance(() => open);
</script>

<Field {label} {help} {error} {required} class={styles.root({ class: className })}>
  {#snippet children(control)}
    <Bits.Root
      type="single"
      items={[...options]}
      {name}
      {required}
      {disabled}
      {inputValue}
      bind:value
      bind:open
      onValueChange={handleValueChange}
      onOpenChange={onopenchange}
    >
      <div class={styles.control()} data-combobox>
        <Bits.Input
          {...portal.anchor}
          {...control}
          {placeholder}
          oninput={handleInput}
          class={input({ size: controlSize, class: styles.input({ class: inputClass }) })}
        />
      </div>

      <Bits.Portal to={portal.container}>
        <Bits.Content
          sideOffset={8}
          collisionPadding={8}
          avoidCollisions={true}
          {...portal.attributes}
          data-combobox-surface
          data-avoid-collisions="true"
          class={popover({ class: styles.content({ class: contentClass }) })}
        >
          <Bits.Viewport>
            {#each filteredOptions as option (option.value)}
              <Bits.Item
                value={option.value}
                label={option.label}
                disabled={option.disabled}
                class={styles.item({ class: itemClass })}
              >
                <span class="min-w-0 break-words">{option.label}</span>
                {#if option.value === value}
                  <span class={styles.indicator()} aria-hidden="true">✓</span>
                  <span class="sr-only">Selected</span>
                {/if}
              </Bits.Item>
            {:else}
              <span class={styles.empty()} data-combobox-empty>No matching options</span>
            {/each}
          </Bits.Viewport>
        </Bits.Content>
      </Bits.Portal>
    </Bits.Root>
  {/snippet}
</Field>
