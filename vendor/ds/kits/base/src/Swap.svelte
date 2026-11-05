<!-- Two caller-owned faces over the canonical ToggleButton pressed contract. -->
<script lang="ts">
  import { untrack, type Snippet } from "svelte";
  import type { HTMLButtonAttributes } from "svelte/elements";
  import ToggleButton from "./ToggleButton.svelte";
  import { warnDeprecated } from "./deprecation";
  import { swap, swapFace } from "./swap.variants";

  interface Props extends Omit<HTMLButtonAttributes, "aria-pressed" | "children" | "class" | "onclick" | "onchange" | "type"> {
    /**
     * Stable accessible name; changing faces never changes the control's purpose. A face that shows
     * words shows words of `label`, in order, ideally its first ones (WCAG 2.5.3, Label in Name):
     * swap a glyph or an emphasis, not the words. A control whose words change is a Button.
     */
    label: string;
    /** Which face is visible, bindable for controlled consumers. */
    swapped?: boolean;
    /** Initial uncontrolled face; later changes do not replace the user's choice. */
    defaultSwapped?: boolean;
    /** Content shown in the initial, unpressed state. Defaults to `label`. */
    off?: Snippet;
    /** Content shown in the swapped, pressed state. Defaults to `label`. */
    on?: Snippet;
    /** Disables the control, so activation neither swaps the face nor reports a change. */
    disabled?: boolean;
    /**
     * Extra classes merged onto the underlying ToggleButton, over the Swap's own one-cell layout.
     */
    class?: string;
    /** Called with the click event on activation, before `onswappedchange`. */
    onclick?: HTMLButtonAttributes["onclick"];
    /** Reports the next state after activation (ADR 0026: `on<state>change` for `bind:<state>`). */
    onswappedchange?: (swapped: boolean) => void;
    /** @deprecated Renamed `onswappedchange` (ADR 0026); removed next release. */
    onchange?: (swapped: boolean) => void;
  }

  let {
    label,
    swapped = $bindable(),
    defaultSwapped = false,
    off,
    on,
    disabled = false,
    class: className,
    onclick,
    onswappedchange,
    onchange,
    ...rest
  }: Props = $props();

  const initial = untrack(() => defaultSwapped);
  let local = $state(initial);
  const current = $derived(swapped ?? local);

  function activate(event: MouseEvent): void {
    const next = current;
    onclick?.(event as Parameters<NonNullable<HTMLButtonAttributes["onclick"]>>[0]);
    onswappedchange?.(next);
    onchange?.(next);
  }

  $effect(() => {
    if (onchange) warnDeprecated("Swap", "onchange", "onswappedchange");
  });
</script>

<ToggleButton
  {...rest}
  {label}
  {disabled}
  pressed={current}
  onpressedchange={next => { local = next; swapped = next; }}
  class={swap({ class: className })}
  data-swap
  onclick={activate}
>
  <span class={swapFace()}>
  {#if current}
    {#if on}{@render on()}{:else}{label}{/if}
  {:else}
    {#if off}{@render off()}{:else}{label}{/if}
  {/if}
  </span>
</ToggleButton>
