<script lang="ts">
  import type { Component } from "svelte";
  import Avatar from "../../src/Avatar.svelte";
  import ToggleGroup from "../../src/ToggleGroup.svelte";
  import AppearanceSwitch from "../../src/AppearanceSwitch.svelte";
  let { name, options = {} }: { name: string; options?: Record<string, any> } = $props();
  const adapters: Record<string, Component<any>> = { Avatar, ToggleGroup, AppearanceSwitch };
  const Adapter = $derived(adapters[name]!);
  export function update(next: Record<string, any>) { options = next; }
</script>
<form><Adapter {...options} /></form>
