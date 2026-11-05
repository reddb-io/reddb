<!-- A pressed-state control composed from the canonical native Button. -->
<script lang="ts">
  import { untrack } from "svelte";
  import { nextPressed } from "./core/native-controls";
  import type { Snippet } from "svelte";
  import type { HTMLButtonAttributes } from "svelte/elements";
  import Button from "./Button.svelte";
  import { toggleButton } from "./toggle-button.variants";

  interface Props extends Omit<HTMLButtonAttributes, "aria-pressed" | "children" | "class" | "onclick" | "type"> {
    /** Required visible and accessible control name. */
    label: string;
    /** Current pressed state, bindable for controlled consumers. */
    pressed?: boolean;
    /** Initial uncontrolled state; later changes do not replace the user's choice. */
    defaultPressed?: boolean;
    /** Group composition can prohibit removing its sole selection. */
    allowUnpress?: boolean;
    /** Disables the button, so activation neither changes `pressed` nor reports a change. */
    disabled?: boolean;
    /** Extra classes merged onto the underlying Button, over the pressed-state styling. */
    class?: string;
    /**
     * Optional visual content; `label` remains the stable accessible name, so words it shows must
     * appear, in order, in `label`, ideally first (WCAG 2.5.3, Label in Name).
     */
    children?: Snippet;
    /**
     * Called with the click event after the pressed state has been updated; skipped while
     * disabled.
     */
    onclick?: HTMLButtonAttributes["onclick"];
    /** Called with the next pressed state when activation changes it (ADR 0026). */
    onpressedchange?: (pressed: boolean) => void;
  }

  let {
    label,
    pressed = $bindable(),
    defaultPressed = false,
    allowUnpress = true,
    disabled = false,
    class: className,
    children,
    onclick,
    onpressedchange,
    ...rest
  }: Props = $props();

  const initial = untrack(() => defaultPressed);
  let local = $state(initial);
  const current = $derived(pressed ?? local);

  function activate(event: MouseEvent): void {
    if (disabled) return;
    const next = nextPressed(current, allowUnpress);
    if (next !== current) {
      local = next;
      pressed = next;
      onpressedchange?.(next);
    }
    onclick?.(event as Parameters<NonNullable<HTMLButtonAttributes["onclick"]>>[0]);
  }
</script>

<Button
  {...rest}
  {disabled}
  variant="secondary"
  class={toggleButton({ pressed: current, class: className })}
  aria-pressed={current}
  aria-label={children ? label : undefined}
  data-state={current ? "on" : "off"}
  data-toggle-button
  onclick={activate}
>
  {#if children}{@render children()}{:else}{label}{/if}
</Button>
