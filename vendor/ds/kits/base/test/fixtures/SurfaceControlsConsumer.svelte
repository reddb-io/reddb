<script lang="ts">
  import {
    AppearanceSwitch,
    ScrollArea,
    SectionHeading,
    Swap,
  } from "@reddb-io/design-system/base";

  const themes = [
    { value: "application", label: "Application" },
    { value: "marketing", label: "Marketing" },
  ] as const;
</script>

<div data-appearance-scope data-theme="marketing" data-color-scheme="dark" data-density="compact">
  <SectionHeading title="Deployments" description="Recent activity" />

  <ScrollArea label="Recent deployments" class="max-h-24">
    <p>Production</p>
    <p>Preview</p>
  </ScrollArea>

  <Swap label="Details for this deployment">
    {#snippet off()}▸ Details{/snippet}
    {#snippet on()}▾ Details{/snippet}
  </Swap>

  <AppearanceSwitch
    label="Theme"
    axis="theme"
    options={themes}
    value="application"
    persistKey="reddb:test:theme"
  />
</div>
