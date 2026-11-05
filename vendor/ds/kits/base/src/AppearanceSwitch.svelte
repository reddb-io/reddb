<!-- One persisted appearance-axis selector composed from canonical Field and Select. -->
<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { appearancePersistence, appearanceValue, commitAppearance, readAppearance, type AppearanceSwitchContract } from "./core/appearance-switch";
  import type { HTMLSelectAttributes } from "svelte/elements";
  import Field from "./Field.svelte";
  import Select from "./Select.svelte";
  import { appearanceStorageKey, type AppearanceStorage } from "./appearance-switch.behavior";

  interface Props extends AppearanceSwitchContract, Omit<HTMLSelectAttributes, "children" | "class" | "onchange" | "value" | "defaultValue" | "multiple"> {
    /** Extra classes merged onto the canonical Select. */
    class?: string;
    /** Reports a user-selected value after it has been applied and persisted when available. */
    onvaluechange?: (value: string) => void;
  }

  let {
    label,
    axis,
    options,
    value = $bindable(),
    defaultValue,
    root,
    storage,
    persistKey,
    class: className,
    onvaluechange,
    ...rest
  }: Props = $props();

  const initial = untrack(() => appearanceValue(options, value, defaultValue));
  let local = $state(initial);
  let initialized = $state(false);
  const selected = $derived(appearanceValue(options, value ?? local));
  const key = $derived(persistKey ?? appearanceStorageKey(axis));
  let scope: { axis: string; key: string; storage?: AppearanceStorage } | undefined;

  function commit(next: string): void {
    if (!options.some(option => !option.disabled && option.value === next)) return;
    local = next; value = next;
    commitAppearance(root ?? globalThis.document?.documentElement, axis, next, appearancePersistence(storage), key);
    onvaluechange?.(next);
  }
  onMount(() => { initialized = true; });
  $effect(() => {
    if (!initialized) return;
    const persistence = appearancePersistence(storage);
    const changed = !scope || scope.axis !== axis || scope.key !== key || scope.storage !== storage;
    scope = { axis, key, storage };
    const next = changed ? appearanceValue(options, readAppearance(persistence, key), selected) : selected;
    if (next !== undefined && next !== selected) { local = next; value = next; }
    commitAppearance(root ?? globalThis.document?.documentElement, axis, next, persistence, key);
  });
</script>

<Field {label} id={rest.id ?? undefined}>
  {#snippet children(control)}
    <Select
      {...rest}
      {...control}
      data-appearance-switch
      data-appearance-axis={axis}
      class={className}
      bind:value={() => selected, next => { local = String(next ?? ""); value = local; }}
      defaultValue={initial}
      onvaluechange={commit}
    >
      {#each options as option (option.value)}
        <option value={option.value} disabled={option.disabled}>{option.label}</option>
      {/each}
    </Select>
  {/snippet}
</Field>
