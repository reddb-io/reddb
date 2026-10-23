<!-- Accessible tab selection and keyboard navigation over caller-owned panel content. -->
<script lang="ts">
  import type { TabsContract } from "./core/contracts/tabs";
  import { tick, type Snippet } from "svelte";
  import { Tabs as Bits } from "bits-ui";
  import Button from "./Button.svelte";
  import { warnDeprecated } from "./deprecation";
  import { tabs, type TabItem } from "./tabs.variants";

  interface Props extends TabsContract {
    /** Called with the newly selected value (ADR 0026's value dialect). */
    onvaluechange?: (value: string) => void;
    /** @deprecated Renamed `onvaluechange` (ADR 0026); removed next release. */
    onchange?: (value: string) => void;
    /**
     * Panel content, rendered once per item inside that tab's panel; receives `{ value }`, the
     * tab's value, to choose what to show.
     */
    children?: Snippet<[{ value: string }]>;
    /** Extra classes, merged over the root slot's own. */
    class?: string;
  }

  let {
    label,
    items,
    value = $bindable(""),
    orientation = "horizontal",
    disabled = false,
    followHash = false,
    onvaluechange,
    onchange,
    children,
    class: className,
    listClass,
    triggerClass,
    contentClass,
  }: Props = $props();

  const styles = tabs();
  const ownerId = $props.id();
  let root = $state<HTMLElement>();
  const selected = $derived(
    items.some((item) => !item.disabled && item.value === value)
      ? value
      : (items.find((item) => !item.disabled)?.value ?? ""),
  );

  function select(next: string): void {
    if (next === selected) return;
    value = next;
    onvaluechange?.(next);
    onchange?.(next);
  }

  $effect(() => {
    if (onchange) warnDeprecated("Tabs", "onchange", "onvaluechange");
  });

  async function selectFragment(): Promise<void> {
    if (!root || !location.hash) return;
    const targetId = decodeURIComponent(location.hash.slice(1));
    const target = document.getElementById(targetId);
    if (!target || !root.contains(target)) return;
    const panel = [...root.querySelectorAll<HTMLElement>("[data-tabs-owner]")]
      .filter((candidate) => candidate.dataset.tabsOwner === ownerId)
      .find((candidate) => candidate.contains(target));
    const next = panel?.dataset.tabValue;
    if (!next || !items.some((item) => item.value === next && !item.disabled)) return;
    select(next);
    await tick();
    await new Promise<void>((resolve) => setTimeout(resolve));
    const visibleTarget = document.getElementById(targetId);
    visibleTarget?.focus({ preventScroll: true });
    if (typeof visibleTarget?.scrollIntoView === "function") {
      visibleTarget.scrollIntoView({ block: "nearest" });
    }
  }

  $effect(() => {
    if (!followHash || !root || typeof window === "undefined") return;
    const handleHash = () => void selectFragment();
    handleHash();
    window.addEventListener("hashchange", handleHash);
    return () => window.removeEventListener("hashchange", handleHash);
  });
</script>

<div
  bind:this={root}
  data-tabs
  aria-label={label}
  class={styles.root({ class: className })}
>
  <Bits.Root
    value={selected}
    onValueChange={select}
    {orientation}
    activationMode="automatic"
    loop={true}
    {disabled}
    aria-label={label}
  >
    <Bits.List aria-label={label} class={styles.list({ class: listClass })}>
      {#each items as item (item.value)}
        <Bits.Trigger value={item.value} disabled={item.disabled}>
          {#snippet child({ props })}
            <Button
              {...props}
              variant="ghost"
              size="sm"
              class={styles.trigger({ class: triggerClass })}
            >{item.label}</Button>
          {/snippet}
        </Bits.Trigger>
      {/each}
    </Bits.List>

    {#each items as item (item.value)}
      <Bits.Content
        value={item.value}
        class={styles.content({ class: contentClass })}
        tabindex={0}
        data-tabs-owner={ownerId}
        data-tab-value={item.value}
      >
        {@render children?.({ value: item.value })}
      </Bits.Content>
    {/each}
  </Bits.Root>
</div>
