<script lang="ts">
  import {
    CommandPalette,
    type CommandPaletteCommand,
  } from "@reddb-io/design-system/app";

  let selected = $state("none");
  const commands: readonly CommandPaletteCommand[] = [
    { id: "settings", label: "Open settings", onselect: () => (selected = "settings") },
    { id: "deploy", label: "Deploy application", onselect: () => (selected = "deploy") },
  ];
</script>

<div data-theme="base" data-color-scheme="dark" data-density="compact" data-motion="reduced">
  <CommandPalette
    triggerLabel="Search commands"
    contentLabel="Global commands"
    label="Command"
    {commands}
  />
  <output aria-live="polite">Selected command: {selected}</output>
</div>
