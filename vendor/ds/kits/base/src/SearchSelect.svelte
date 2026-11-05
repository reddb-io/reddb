<!-- A closed choice control with search inside its Popover and optional multiple selection. -->
<script lang="ts">
  import { portalAppearance } from "./portal-appearance.svelte";
  import type { SearchSelectContract } from "./core/contracts/searchselect";
  import { tick } from "svelte";
  import { focusMovedOutside } from "./core/focus-scope";
  import { Combobox as Bits, Popover } from "bits-ui";
  import Field from "./Field.svelte";
  import Button from "./Button.svelte";
  import { input } from "./input.variants";
  import { popover } from "./popover.variants";
  import type { InputSize } from "./input.variants";
  import { searchSelect, searchSelectOptions, type SearchSelectOption } from "./search-select.variants";

  interface Common extends SearchSelectContract {
    /** Extra classes merged onto the Field root. */
    class?: string;
    /** Called when the Popover opens or closes. */
    onopenchange?: (open: boolean) => void;
  }
  interface Single extends Common {
    /** Enable multiple selection and removable chips. Defaults to false. */
    multiple?: false;
    /** Selected string in single mode, or string array in multiple mode; use bind:value. */
    value?: string;
    /** Called with a string in single mode or string array in multiple mode, including clearing and chip removal. */
    onvaluechange?: (value: string) => void;
  }
  interface Multiple extends Common {
    /** Enable multiple selection and removable chips. */
    multiple: true;
    /** Selected values; use bind:value to keep them synchronized. */
    value?: string[];
    /** Called with selected values after selection, removal or clearing. */
    onvaluechange?: (value: string[]) => void;
  }
  type Props = Single | Multiple;

  let {
    label, options, multiple = false, value = $bindable(multiple ? [] : ""), name,
    help, error, required = false, disabled = false,
    placeholder = "Choose an option", searchPlaceholder = "Search options",
    emptyText = "No matching options", requiredMessage = "Choose at least one option.",
    clearable = true, open = $bindable(false), controlSize = "md",
    class: className, triggerClass, contentClass, onvaluechange, onopenchange,
  }: Props = $props();
  const styles = searchSelect();
  const uid = $props.id();
  const listId = `${uid}-listbox`;
  let query = $state("");
  let tabExit = false;
  let invalid = $state(false);
  let nativeSelect = $state<HTMLSelectElement>();
  let anchor = $state<HTMLDivElement>();
  const selected = $derived(Array.isArray(value) ? value : value ? [value] : []);
  const selectedLabels = $derived(selected.map(id => options.find(option => option.value === id)?.label ?? id));
  const summary = $derived(selectedLabels.length > 1 ? `${selectedLabels.length} selected` : selectedLabels[0] ?? placeholder);
  const filtered = $derived(searchSelectOptions(options, query));
  const fieldError = $derived(error ?? (invalid && !selected.length ? requiredMessage : undefined));

  $effect(() => { if (!open) query = ""; });
  $effect(() => { if (disabled && open) changeOpen(false); });
  $effect(() => {
    const form = nativeSelect?.form;
    const reset = () => (invalid = false);
    form?.addEventListener("reset", reset);
    return () => form?.removeEventListener("reset", reset);
  });
  function nativeChange(event: Event): void {
    const select = event.currentTarget as HTMLSelectElement;
    change(multiple ? [...select.selectedOptions].map(option => option.value) : select.value);
  }
  function nativeInvalid(event: Event): void {
    event.preventDefault(); invalid = true;
    const select = event.currentTarget as HTMLSelectElement;
    const firstInvalid = select.form ? [...select.form.elements].find(element => "willValidate" in element && (element as HTMLInputElement).willValidate && !(element as HTMLInputElement).validity.valid) : select;
    if (firstInvalid === select) select.parentElement?.querySelector<HTMLButtonElement>("[data-search-select-trigger]")?.focus();
  }
  function change(next: string | string[]): void {
    value = next;
    invalid = false;
    if (multiple) (onvaluechange as ((value: string[]) => void) | undefined)?.(Array.isArray(next) ? next : []);
    else (onvaluechange as ((value: string) => void) | undefined)?.(typeof next === "string" ? next : "");
  }
  function changeOpen(next: boolean): void {
    open = next;
    query = "";
    onopenchange?.(next);
  }
  async function choose(next: string | string[]): Promise<void> {
    const changed = Array.isArray(next) ? next.find(id => !selected.includes(id)) ?? selected.find(id => !next.includes(id)) : next;
    change(next);
    if (!multiple) { changeOpen(false); return; }
    // Bits writes the picked label after reporting selection. Mirror that
    // write, then clear it so the next search starts with the complete list.
    query = options.find(option => option.value === changed)?.label ?? "";
    await tick();
    query = "";
  }
  function searchKeydown(event: KeyboardEvent): void {
    if (event.key !== "Tab") return;
    event.stopPropagation();
    tabExit = !event.shiftKey;
    if (event.shiftKey) event.preventDefault();
    else nativeSelect?.parentElement?.querySelector<HTMLButtonElement>("[data-search-select-trigger]")?.focus();
    if (open) changeOpen(false);
  }
  function remove(id: string, event: MouseEvent): void {
    const current = event.currentTarget as HTMLElement;
    // Move focus before removing the focused chip, so it never falls to the document body.
    current.closest("[data-search-select]")?.querySelector<HTMLButtonElement>("[data-search-select-trigger]")?.focus();
    change(multiple ? selected.filter(candidate => candidate !== id) : "");
  }
  function clear(event: MouseEvent): void {
    (event.currentTarget as HTMLElement).closest("[data-search-select]")?.querySelector<HTMLButtonElement>("[data-search-select-trigger]")?.focus();
    change(multiple ? [] : "");
  }
  const portal = portalAppearance(() => open);
</script>

<Field {label} {help} error={fieldError} {required} class={styles.root({ class: className })}>
  {#snippet children(control)}
    <div class="grid gap-[var(--reddb-spatial-gap-sm)]" data-search-select data-multiple={multiple}>
      {#if multiple}
        <select bind:this={nativeSelect} bind:value multiple {name} {disabled} {required} tabindex="-1" aria-hidden="true" class="sr-only" onchange={nativeChange} oninvalid={nativeInvalid}>
          {@render nativeOptions()}
        </select>
      {:else}
        <select bind:this={nativeSelect} bind:value {name} {disabled} {required} tabindex="-1" aria-hidden="true" class="sr-only" onchange={nativeChange} oninvalid={nativeInvalid}>
          <option value="">{placeholder}</option>{@render nativeOptions()}
        </select>
      {/if}
      {#snippet nativeOptions()}{#each options as option (option.value)}<option value={option.value} disabled={option.disabled}>{option.label}</option>{/each}{/snippet}
      <div {...portal.anchor} bind:this={anchor} class={styles.row()}>
        <Popover.Root bind:open onOpenChange={changeOpen}>
          <Popover.Trigger>
            {#snippet child({ props })}
              <Button {...props} id={control.id} {disabled} size={controlSize}
                aria-label={`${summary}, ${label}${required ? ", required" : ""}`} aria-describedby={control["aria-describedby"]}
                aria-invalid={control["aria-invalid"]} aria-errormessage={control["aria-errormessage"]} 
                class={input({ size: controlSize, class: styles.trigger({ class: triggerClass }) })}
                onkeydown={(event: KeyboardEvent) => { if (event.key === "ArrowDown" || event.key === "ArrowUp") { event.preventDefault(); changeOpen(true); } }}
                data-search-select-trigger>
                <span class={styles.summary({ class: !selected.length ? styles.placeholder() : undefined })}>{summary}</span>
                <span aria-hidden="true">⌄</span>
              </Button>
            {/snippet}
          </Popover.Trigger>
          <Popover.Portal to={portal.container}>
            <Popover.Content {...portal.attributes} aria-label={label} sideOffset={8} collisionPadding={8} align="start" trapFocus={false} customAnchor={anchor}
              onCloseAutoFocus={(event) => { if (tabExit || focusMovedOutside(anchor, "[data-search-select-surface]")) event.preventDefault(); tabExit = false; }}
              class={popover({ class: styles.content({ class: contentClass }) })} data-search-select-surface>
              {#if multiple}
                <Bits.Root type="multiple" value={selected} onValueChange={choose} inputValue={query} items={[...options]} open onOpenChange={(next) => { if (!next && open) changeOpen(false); }}>
                  {@render choices()}
                </Bits.Root>
              {:else}
                <Bits.Root type="single" value={typeof value === "string" ? value : ""} onValueChange={choose} inputValue={query} items={[...options]} allowDeselect={false} open onOpenChange={(next) => { if (!next && open) changeOpen(false); }}>
                  {@render choices()}
                </Bits.Root>
              {/if}
            </Popover.Content>
          </Popover.Portal>
        </Popover.Root>
        {#if clearable && selected.length}
          <Button variant="ghost" size={controlSize} {disabled} class={styles.clear()} aria-label={`Clear ${label}`} title={`Clear ${label}`} onclick={clear} data-search-select-clear><span aria-hidden="true">×</span></Button>
        {/if}
      </div>
      {#if multiple && selected.length}
        <ul class={styles.chips()} aria-label={`Selected ${label}`}>
          {#each selected as id, index (id)}
            <li class="min-w-0 max-w-full">
              <Button variant="secondary" size="sm" class={styles.chip()} {disabled} aria-label={`${selectedLabels[index]}, remove from ${label}`} onclick={(event: MouseEvent) => remove(id, event)} data-search-select-chip>
                <span class="min-w-0 truncate">{selectedLabels[index]}</span><span aria-hidden="true">×</span>
              </Button>
            </li>
          {/each}
        </ul>
      {/if}
      <span class="sr-only" role="status">{selectedLabels.length ? selectedLabels.join(", ") : "No options selected"}</span>
    </div>
    {#snippet choices()}
      <Bits.Input aria-label={`Search ${label}`} aria-controls={listId} placeholder={searchPlaceholder} class={input({ size: controlSize, class: styles.search() })} onkeydown={searchKeydown}
        oninput={(event) => (query = event.currentTarget.value)}>
        {#snippet child({ props })}
          <input {...props} aria-expanded={filtered.length > 0} aria-controls={filtered.length ? listId : undefined} aria-activedescendant={filtered.length && typeof props["aria-activedescendant"] === "string" ? props["aria-activedescendant"] : undefined} />
        {/snippet}
      </Bits.Input>
      {#if filtered.length}
      <Bits.ContentStatic id={listId} aria-label={label} class={styles.list()} onEscapeKeydown={(event) => { event.preventDefault(); changeOpen(false); }}>
        <Bits.Viewport>
          {#each filtered as option (option.value)}
            <Bits.Item value={option.value} label={option.label} disabled={option.disabled} aria-disabled={option.disabled || undefined} class={styles.item()}>
              <span class={styles.option()}>{option.label}{#if option.description}<span class={styles.description()}>{option.description}</span>{/if}</span>
              {#if selected.includes(option.value)}<span aria-hidden="true">✓</span><span class="sr-only">Selected</span>{/if}
            </Bits.Item>
          {/each}
        </Bits.Viewport>
      </Bits.ContentStatic>
      {:else}<span class={styles.empty()} role="status">{emptyText}</span>{/if}
    {/snippet}
  {/snippet}
</Field>
