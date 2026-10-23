<script lang="ts">
  import type { Component } from "svelte";
  import ToggleButton from "../../src/ToggleButton.svelte";
  import Rating from "../../src/Rating.svelte";
  import FileInput from "../../src/FileInput.svelte";
  let { name, options = {} }: { name: string; options?: Record<string, any> } = $props();
  const adapters: Record<string, Component<any>> = { ToggleButton, Rating, FileInput };
  const Adapter = $derived(adapters[name]!);
  export function update(next: Record<string, unknown>) { options = next; }
</script>
<form><Adapter {...options} /></form>
