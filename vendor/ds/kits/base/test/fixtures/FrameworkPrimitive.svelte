<script lang="ts">
  import type { Component } from "svelte";
  import Badge from "../../src/Badge.svelte";
  import Button from "../../src/Button.svelte";
  import Container from "../../src/Container.svelte";
  import Divider from "../../src/Divider.svelte";
  import Heading from "../../src/Heading.svelte";
  import Label from "../../src/Label.svelte";
  import LoadingIndicator from "../../src/LoadingIndicator.svelte";
  import Stack from "../../src/Stack.svelte";
  import Text from "../../src/Text.svelte";
  // This fixture deliberately accepts each adapter's own native props.
  const adapters: Record<string, Component<any>> = { Badge, Button, Container, Divider, Heading, Label, LoadingIndicator, Stack, Text };
  const { name, options = {} }: { name: string; options?: Record<string, unknown> } = $props();
  const Adapter = $derived(adapters[name]!);
</script>
<Adapter {...options}>Content</Adapter>
