<script lang="ts">
  import type { Component } from "svelte";
  import NavigationScope from "../../src/NavigationScope.svelte";
  import LinkPreview from "../../src/LinkPreview.svelte";
  import Dialog from "../../src/Dialog.svelte";
  import Drawer from "../../src/Drawer.svelte";
  import AlertDialog from "../../src/AlertDialog.svelte";
  import Disclosure from "../../src/Disclosure.svelte";
  import Accordion from "../../src/Accordion.svelte";
  import Tabs from "../../src/Tabs.svelte";
  import Combobox from "../../src/Combobox.svelte";
  import Popover from "../../src/Popover.svelte";
  import DropdownMenu from "../../src/DropdownMenu.svelte";
  import Tooltip from "../../src/Tooltip.svelte";
  import DateField from "../../src/DateField.svelte";
  import TimeField from "../../src/TimeField.svelte";
  import DateRangeField from "../../src/DateRangeField.svelte";
  import TimeRangeField from "../../src/TimeRangeField.svelte";
  import Calendar from "../../src/Calendar.svelte";
  import RangeCalendar from "../../src/RangeCalendar.svelte";
  import DatePicker from "../../src/DatePicker.svelte";
  import DateRangePicker from "../../src/DateRangePicker.svelte";
  import SearchSelect from "../../src/SearchSelect.svelte";
  import Carousel from "../../src/Carousel.svelte";
  import Navbar from "../../src/Navbar.svelte";
  import NavigationMenu from "../../src/NavigationMenu.svelte";
  let { name, options = {}, changed = (_value: any) => {} }: { name: string; options?: Record<string, any>; changed?: (value: any) => void } = $props();
  const adapters: Record<string, Component<any>> = { Dialog, Drawer, AlertDialog, Disclosure, Accordion, Tabs, Combobox, Popover, DropdownMenu, Tooltip, LinkPreview, NavigationScope, DateField, TimeField, DateRangeField, TimeRangeField, Calendar, RangeCalendar, DatePicker, DateRangePicker, SearchSelect, Carousel, Navbar, NavigationMenu };
  const Adapter = $derived(adapters[name]!);
  export function update(next: Record<string, any>) { options = next; }
</script>
<form>
  {#if name === "Accordion" || name === "Tabs"}
    <Adapter {...options} onvaluechange={changed}>
      {#snippet children(item: { value: string })}<p>{item.value} details</p>{/snippet}
    </Adapter>
  {:else if name === "Dialog" || name === "Drawer" || name === "AlertDialog" || name === "Disclosure" || name === "Popover"}
    <Adapter {...options} onopenchange={changed}><button type="button">Review detail</button>{#if name === "Dialog" || name === "Drawer"}<Popover triggerLabel="Nested options" contentLabel="Nested choices"><button type="button">Choose nested option</button></Popover>{/if}</Adapter>
  {:else if name === "DateField" || name === "TimeField" || name === "DateRangeField" || name === "TimeRangeField" || name === "Calendar" || name === "RangeCalendar" || name === "DatePicker" || name === "DateRangePicker" || name === "SearchSelect"}<Adapter {...options} onvaluechange={changed} />
  {:else if name === "NavigationScope"}<Adapter {...options}><a href="/inspect">Inspect request</a><a href="#summary">Summary</a></Adapter>
  {:else if name === "Combobox"}<Combobox {...options} label={options.label} options={options.options} onvaluechange={changed} />
  {:else}<Adapter {...options} />{/if}
</form>
