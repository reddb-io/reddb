<!-- A consumer-owned arrangement and content exercising the disclosure family together. -->
<script lang="ts">
  import { Accordion, Disclosure, Tabs } from "@reddb-io/design-system/base";

  interface Props {
    ontabchange?: (value: string) => void;
    onaccordionchange?: (value: readonly string[]) => void;
    ondisclosurechange?: (open: boolean) => void;
  }

  const { ontabchange, onaccordionchange, ondisclosurechange }: Props = $props();
  const tabs = [
    { value: "summary", label: "Summary" },
    { value: "events", label: "Events" },
    { value: "disabled", label: "Disabled", disabled: true },
  ] as const;
  const sections = [
    { value: "changes", label: "What changed?" },
    { value: "impact", label: "Who is affected?" },
  ] as const;
</script>

{#snippet tabContent(value: string)}
  {#if value === "summary"}<p>Summary content</p>{:else}<p>Event content</p>{/if}
{/snippet}

{#snippet accordionContent(value: string)}
  {#if value === "changes"}<p>Release notes</p>{:else}<p>All operators</p>{/if}
{/snippet}

<div data-disclosure-family-consumer>
  <Tabs
    label="Deployment view"
    items={tabs}
    value="summary"
    onchange={ontabchange}
  >
    {#snippet children({ value })}{@render tabContent(value)}{/snippet}
  </Tabs>

  <Accordion
    label="Release questions"
    items={sections}
    expanded={["changes"]}
    onchange={onaccordionchange}
  >
    {#snippet children({ value })}{@render accordionContent(value)}{/snippet}
  </Accordion>

  <Disclosure label="Advanced details" open onchange={ondisclosurechange}>
    <p>Caller-owned detail</p>
  </Disclosure>

  <div
    data-appearance-scope
    data-theme="marketing"
    data-color-scheme="dark"
    data-density="compact"
  >
    <Tabs label="Nested tabs" items={tabs}>
      {#snippet children({ value })}<p>Nested {value}</p>{/snippet}
    </Tabs>
    <Accordion label="Nested accordion" items={sections}>
      {#snippet children({ value })}<p>Nested {value}</p>{/snippet}
    </Accordion>
    <Disclosure label="Nested disclosure"><p>Nested detail</p></Disclosure>
  </div>
</div>
