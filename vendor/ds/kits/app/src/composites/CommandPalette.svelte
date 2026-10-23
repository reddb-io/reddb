<!-- Global command search composed from the canonical Popover and Combobox contracts. -->
<script lang="ts">
  import type { CommandPaletteContract } from "../core/contracts/commandpalette";
  import { Combobox, Popover, type ComboboxOption } from "@reddb-io/design-system/base";
  import type { CommandPaletteCommand } from "./command-palette.behavior";
  import { commandPalette } from "./command-palette.variants";

  interface Props extends CommandPaletteContract {
    /** Called with the new open state whenever the palette opens or closes. */
    onopenchange?: (open: boolean) => void;
    /**
     * Extra classes, merged over the root slot's own, onto the element wrapping the trigger and
     * Popover.
     */
    class?: string;
    /**
     * Called with the chosen command after its own `onselect` has run and just before the palette
     * closes.
     */
    onselect?: (command: CommandPaletteCommand) => void;
  }

  let {
    triggerLabel,
    contentLabel,
    label,
    commands = [],
    open = $bindable(false),
    onopenchange,
    disabled = false,
    class: className,
    contentClass,
    inputClass,
    onselect,
  }: Props = $props();

  const slots = commandPalette();
  let value = $state("");
  const options = $derived<readonly ComboboxOption[]>(
    commands.map(({ id, label: commandLabel, disabled: commandDisabled }) => ({
      value: id,
      label: commandLabel,
      disabled: commandDisabled,
    })),
  );

  function choose(id: string): void {
    const command = commands.find((candidate) => candidate.id === id);
    if (command === undefined) return;
    command.onselect?.();
    onselect?.(command);
    open = false;
    // Closing on a command is the palette's own doing, which the Popover does
    // not report; the open-state callback still hears it.
    onopenchange?.(false);
    // A command is an action, not a chosen value: forget it, or choosing the
    // same command next time toggles the selection off instead of running it.
    value = "";
  }
</script>

<div data-command-palette class={slots.root({ class: className })}>
  <Popover
    {triggerLabel}
    {contentLabel}
    {disabled}
    bind:open
    {onopenchange}
    class={slots.content({ class: contentClass })}
  >
    <Combobox
      {label}
      {options}
      bind:value
      {inputClass}
      class={slots.search()}
      onvaluechange={choose}
    />
  </Popover>
</div>
