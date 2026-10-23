import { mount, unmount, flushSync } from "svelte";
import { act, createElement as h, type ComponentType } from "react";
import { createRoot } from "react-dom/client";
import { renderToString } from "react-dom/server";
import { parseDate, toCalendar, HebrewCalendar } from "@internationalized/date";
import { afterEach, describe, expect, it, vi } from "vitest";
import * as react from "../react/src";
import Fixture from "./fixtures/FrameworkManaged.svelte";
HTMLElement.prototype.scrollIntoView ??= () => {};
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
const cleanups: (() => Promise<void>)[] = [];
afterEach(async () => { while (cleanups.length) await cleanups.pop()!(); });
const items = [{ value: "overview", label: "Overview" }, { value: "disabled", label: "Unavailable", disabled: true }, { value: "details", label: "Details" }];
const defaults = {
 LinkPreview: { href: "#preview", label: "Trace guide", previewLabel: "Guide preview", openDelay: 0, closeDelay: 0 },
 NavigationScope: { navigate: (_href: string, _event: MouseEvent) => {} },
 DateField: { label: "Date", name: "date" }, TimeField: { label: "Time", name: "time" },
 DateRangeField: { label: "Dates", startLabel: "From date", endLabel: "To date", startName: "from", endName: "to" },
 TimeRangeField: { label: "Times", startLabel: "From time", endLabel: "To time", startName: "from", endName: "to" },
 Calendar: { label: "Review date", placeholder: parseDate("2026-06-10") }, RangeCalendar: { label: "Review dates", placeholder: parseDate("2026-06-10") },
 DatePicker: { label: "Date", calendarLabel: "Choose date", placeholder: parseDate("2026-06-10") },
 DateRangePicker: { label: "Dates", startLabel: "Start", endLabel: "End", calendarLabel: "Choose dates", placeholder: parseDate("2026-06-10") },
 SearchSelect: { label: "Region", name: "region", options: [{ value: "sao", label: "São Paulo", keywords: ["br"] }, { value: "fra", label: "Frankfurt", description: "Europe" }, { value: "off", label: "Unavailable", disabled: true }] },
 Carousel: { label: "Release notes", slides: [{ label: "First" }, { label: "Second" }] },
 Navbar: { label: "Review navigation", collapse: "collapsed" as const, links: [{ id: "overview", label: "Overview", href: "#overview" }] },
 NavigationMenu: { label: "Review destinations", items: [{ id: "learn", label: "Learn", links: [{ id: "guide", label: "Guide", href: "/guide" }] }] },
 Dialog: { title: "Review", triggerLabel: "Open review" }, Drawer: { title: "Review", triggerLabel: "Open review" }, AlertDialog: { title: "Review", triggerLabel: "Open review", confirmLabel: "Delete" },
 Disclosure: { label: "Review" }, Accordion: { label: "Review", items }, Tabs: { label: "Review", items },
 Popover: { triggerLabel: "Open review", contentLabel: "Review" },
 Combobox: { label: "Region", name: "region", options: items },
 DropdownMenu: { triggerLabel: "Actions", contentLabel: "Review actions", items: [{ id: "inspect", label: "Inspect" }] },
 Tooltip: { label: "Help", content: "Review help" },
};
type Name = keyof typeof defaults;
async function interact(fn: () => void) { await act(async () => { fn(); flushSync(); }); }
async function render(name: Name, extra: Record<string, any> = {}) {
 const a = document.createElement("div"), b = document.createElement("div"); document.body.append(a, b);
 const changed = { svelte: vi.fn(), react: vi.fn() };
 const native = (extra: Record<string, any>) => Object.fromEntries(Object.entries({ ...defaults[name], ...extra }).map(([key, value]) => [key === "className" ? "class" : key.startsWith("on") ? key.toLowerCase() : key, value]));
 const svelte = mount(Fixture, { target: a, props: { name, options: native(extra), changed: changed.svelte } }); flushSync();
 const root = createRoot(b);
 const element = (extra: Record<string, any>) => h("form", null, h(react[name] as ComponentType<any>, { ...defaults[name], ...extra,
  ...(["Accordion", "Tabs", "Combobox", "SearchSelect", "Calendar", "RangeCalendar", "DateField", "TimeField", "DateRangeField", "TimeRangeField", "DatePicker", "DateRangePicker"].includes(name) ? { onValueChange: changed.react } : { onOpenChange: changed.react }),
  ...(["Accordion", "Tabs"].includes(name) ? { children: (item: { value: string }) => h("p", null, `${item.value} details`) } : name === "NavigationScope" ? { children: [h("a", { key: "inspect", href: "/inspect" }, "Inspect request"), h("a", { key: "summary", href: "#summary" }, "Summary")] } : ["Dialog", "Drawer", "AlertDialog", "Disclosure", "Popover"].includes(name) ? { children: h("div", null, h("button", { type: "button" }, "Review detail"), ...(["Dialog", "Drawer"].includes(name) ? [h(react.Popover, { triggerLabel: "Nested options", contentLabel: "Nested choices" }, h("button", { type: "button" }, "Choose nested option"))] : [])) } : {}),
 }));
 await act(() => root.render(element(extra)));
 cleanups.push(async () => { await act(() => root.unmount()); await unmount(svelte); a.remove(); b.remove(); });
 return { a, b, changed, update: async (extra: Record<string, any>) => { flushSync(() => svelte.update(native(extra))); await act(() => root.render(element(extra))); } };
}
for (const flavor of ["svelte", "react"] as const) describe(`${flavor} managed contracts`, () => {
 for (const name of ["Dialog", "Drawer", "AlertDialog"] as const) it(`${name} opens a named top-layer surface, closes with Escape and returns focus`, async () => {
  const r = await render(name), target = flavor === "react" ? r.b : r.a, trigger = target.querySelector<HTMLButtonElement>("[data-dialog-trigger]")!, dialog = target.querySelector("dialog")!;
  trigger.focus(); await interact(() => trigger.click()); expect(dialog.open).toBe(true); expect(document.activeElement?.closest("dialog")).toBe(dialog);
  expect(document.getElementById(dialog.getAttribute("aria-labelledby")!)?.textContent).toBe("Review"); expect(document.documentElement.style.overflow).toBe("hidden");
  await interact(() => dialog.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true })));
  expect(dialog.open).toBe(false); expect(document.activeElement).toBe(trigger); expect(document.documentElement.style.overflow).not.toBe("hidden"); expect(r.changed[flavor].mock.calls.map(call => call[0])).toEqual([true, false]);
 });
 it("opens and collapses one labelled region without changing its trigger", async () => {
  const r = await render("Disclosure"), target = flavor === "react" ? r.b : r.a, trigger = target.querySelector("button")!;
  expect(trigger.getAttribute("aria-expanded")).toBe("false"); await interact(() => trigger.click()); expect(trigger.getAttribute("aria-expanded")).toBe("true");
  const region = target.querySelector('[role="region"]')!; expect(region.textContent).toBe("Review detail"); expect(region.getAttribute("aria-labelledby")).toBe(trigger.id);
  await interact(() => trigger.click()); expect(trigger.getAttribute("aria-expanded")).toBe("false"); expect(r.changed[flavor].mock.calls.map(call => call[0])).toEqual([true, false]);
 });
 it("keeps one expanded item in single mode and excludes disabled choices", async () => {
  const r = await render("Accordion"), target = flavor === "react" ? r.b : r.a, triggers = [...target.querySelectorAll("button")];
  await interact(() => triggers[0]!.click()); expect(triggers[0]!.getAttribute("aria-expanded")).toBe("true");
  await interact(() => triggers[2]!.click()); expect(triggers[0]!.getAttribute("aria-expanded")).toBe("false"); expect(triggers[2]!.getAttribute("aria-expanded")).toBe("true");
  expect(triggers[1]!.disabled).toBe(true); expect(r.changed[flavor].mock.calls.map(call => call[0])).toEqual([["overview"], ["details"]]);
 });
 it("selects enabled tabs, associates their panels and honors a disabled collection", async () => {
  const r = await render("Tabs"), target = flavor === "react" ? r.b : r.a, triggers = [...target.querySelectorAll<HTMLButtonElement>('[role="tab"]')];
  expect(triggers[0]!.getAttribute("aria-selected")).toBe("true"); await interact(() => triggers[2]!.click()); expect(triggers[2]!.getAttribute("aria-selected")).toBe("true");
  const panel = target.querySelector<HTMLElement>(`[id="${triggers[2]!.getAttribute("aria-controls")}"]`)!; expect(panel.textContent).toBe("details details");
  await r.update({ disabled: true }); expect([...target.querySelectorAll<HTMLButtonElement>('[role="tab"]')].every(trigger => trigger.disabled)).toBe(true);
 });
});
for (const name of Object.keys(defaults) as Name[]) it(`renders ${name} natively on the server without opening its overlay`, () => {
 const html = renderToString(h(react[name] as ComponentType<any>, defaults[name])); expect(html.length).toBeGreaterThan(0);
 if (["Dialog", "Drawer", "AlertDialog"].includes(name)) { expect(html).toContain("<dialog"); expect(html).not.toMatch(/<dialog[^>]+ open=/); }
});

async function type(input: HTMLInputElement, value: string) {
 await interact(() => { Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, value); input.dispatchEvent(new Event("input", { bubbles: true })); });
}
for (const flavor of ["svelte", "react"] as const) describe(`${flavor} date, search and navigation contracts`, () => {
 for (const name of ["DateField", "TimeField"] as const) it(`${name} retains partial drafts, submits complete values and resets silently`, async () => {
  const r = await render(name), root = flavor === "react" ? r.b : r.a, controls = [...root.querySelectorAll<HTMLInputElement>('input:not([type="hidden"])')];
  const values = name === "DateField" ? ["2024", "02", "29"] : ["23", "59"], form = root.querySelector("form")!;
  await type(controls[0]!, values[0]!); expect(new FormData(form).get(name === "DateField" ? "date" : "time")).toBeNull();
  for (let i = 1; i < controls.length; i++) await type(controls[i]!, values[i]!);
  expect(new FormData(form).get(name === "DateField" ? "date" : "time")).toBe(values.join(name === "DateField" ? "-" : ":"));
  const calls = r.changed[flavor].mock.calls.length; await interact(() => form.reset()); await act(() => new Promise(resolve => setTimeout(resolve, 10)));
  expect(controls.map(control => control.value)).toEqual(values.map(() => "")); expect(r.changed[flavor]).toHaveBeenCalledTimes(calls);
 });
 it("rejects impossible dates without discarding a partial edit", async () => {
  const r = await render("DateField"), root = flavor === "react" ? r.b : r.a, controls = [...root.querySelectorAll<HTMLInputElement>('input:not([type="hidden"])')];
  for (const [index, value] of ["2023", "02", "29"].entries()) await type(controls[index]!, value);
  expect(new FormData(root.querySelector("form")!).get("date")).toBeNull(); expect(controls[2]!.value).toBe("29");
 });
 it("navigates dates by keyboard and refuses disabled dates", async () => {
  const r = await render("Calendar", { minValue: parseDate("2026-06-09"), isDateDisabled: (date: any) => date.toString() === "2026-06-11" }), root = flavor === "react" ? r.b : r.a;
  const day = (value: string) => root.querySelector<HTMLElement>(`[data-calendar-date="${value}"], [data-bits-day][data-value="${value}"]`)!;
  const start = day("2026-06-10"); start.focus(); await interact(() => start.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true, cancelable: true })));
  expect(document.activeElement).toBe(day("2026-06-12")); await interact(() => day("2026-06-12").click());
  expect(r.changed[flavor].mock.calls.at(-1)?.[0]?.toString()).toBe("2026-06-12");
  expect(day("2026-06-11").getAttribute("aria-disabled") === "true" || day("2026-06-11").hasAttribute("disabled")).toBe(true);
 });
 it("selects an ordered inclusive date range", async () => {
  const r = await render("RangeCalendar"), root = flavor === "react" ? r.b : r.a;
  const day = (value: string) => root.querySelector<HTMLElement>(`[data-calendar-date="${value}"], [data-bits-day][data-value="${value}"]`)!;
  await interact(() => day("2026-06-12").click()); await interact(() => day("2026-06-10").click());
  const next = r.changed[flavor].mock.calls.at(-1)?.[0]; expect(next.start.toString()).toBe("2026-06-10"); expect(next.end.toString()).toBe("2026-06-12");
 });
 it("searches choices by accent-insensitive aliases and submits the selected value", async () => {
  const r = await render("SearchSelect"), root = flavor === "react" ? r.b : r.a;
  await interact(() => root.querySelector<HTMLButtonElement>("[data-search-select-trigger]")!.click());
  const popup = document.querySelector<HTMLElement>("[data-search-select-surface]")!; expect(popup).toBeTruthy();
  await type(popup.querySelector("input")!, "br");
  const options = [...popup.querySelectorAll<HTMLElement>('[role="option"]')]; expect(options.map(option => option.textContent?.trim())).toEqual(["São Paulo"]);
  await interact(() => { options[0]!.dispatchEvent(new MouseEvent("pointerup", { bubbles: true, cancelable: true })); options[0]!.click(); }); expect(new FormData(root.querySelector("form")!).get("region")).toBe("sao");
  expect(r.changed[flavor].mock.calls.at(-1)?.[0]).toBe("sao");
 });
 it("opens the compact navbar, exposes its destinations, and restores focus on Escape", async () => {
  const r = await render("Navbar"), root = flavor === "react" ? r.b : r.a, trigger = root.querySelector<HTMLButtonElement>("[data-navbar-toggle]")!;
  await interact(() => trigger.click()); expect(trigger.getAttribute("aria-expanded")).toBe("true"); expect(root.querySelector<HTMLElement>("[data-navbar-panel]")!.hidden).toBe(false);
  await interact(() => trigger.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }))); expect(trigger.getAttribute("aria-expanded")).toBe("false"); expect(document.activeElement).toBe(trigger);
 });
 it("rotates slides only after an explicit navigation action by default", async () => {
  const r = await render("Carousel"), root = flavor === "react" ? r.b : r.a;
  expect(root.querySelectorAll('[data-carousel-slide]:not([hidden])')).toHaveLength(1);
  await interact(() => root.querySelector<HTMLButtonElement>('[aria-label="Next slide"]')!.click()); expect(root.querySelector("[data-carousel-position]")!.textContent?.trim()).toBe("2 of 2"); expect(root.querySelector("[data-carousel-rotation]")).toBeNull();
 });
 it("invalidates a pending confirmation when its overlay closes and reopens", async () => {
  let finish!: () => void; const confirm = vi.fn(() => new Promise<void>(resolve => { finish = resolve; }));
  const r = await render("AlertDialog", { onConfirm: confirm }), root = flavor === "react" ? r.b : r.a, trigger = root.querySelector<HTMLButtonElement>("[data-dialog-trigger]")!, dialog = root.querySelector("dialog")!;
  await interact(() => trigger.click()); await interact(() => { root.querySelector<HTMLButtonElement>("[data-alert-dialog-confirm]")!.click(); root.querySelector<HTMLButtonElement>("[data-alert-dialog-confirm]")!.click(); }); expect(confirm).toHaveBeenCalledTimes(1);
  await interact(() => dialog.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }))); await interact(() => trigger.click()); await interact(() => finish()); expect(dialog.open).toBe(true);
 });
});

for (const flavor of ["svelte", "react"] as const) describe(`${flavor} anchored and routed behavior`, () => {
 it("opens a named popover, focuses its content, then returns focus on Escape", async () => {
  const r = await render("Popover"), root = flavor === "react" ? r.b : r.a, trigger = root.querySelector<HTMLButtonElement>("[data-popover-trigger]")!;
  trigger.focus(); await interact(() => trigger.click()); const popup = document.querySelector<HTMLElement>("[data-popover-surface]")!;
  expect(popup.getAttribute("aria-label")).toBe("Review");
  // The managed native adapter schedules focus after positioning. Assert its
  // observable result instead of depending on a particular animation frame.
  await vi.waitFor(() => expect(popup.contains(document.activeElement)).toBe(true));
  await interact(() => document.activeElement!.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true })));
  await vi.waitFor(() => {
   expect(trigger.getAttribute("aria-expanded")).toBe("false");
   expect(document.activeElement).toBe(trigger);
  });
 });
 it("keeps menu rows named and activates one enabled command", async () => {
  const action = vi.fn(), r = await render("DropdownMenu", { items: [{ id: "inspect", label: "Inspect", onselect: action }, { id: "locked", label: "Locked", disabled: true }] }), root = flavor === "react" ? r.b : r.a;
  await interact(() => root.querySelector<HTMLButtonElement>("[data-dropdown-menu-trigger]")!.click());
  const popup = document.querySelector<HTMLElement>("[data-dropdown-menu-surface]")!, items = [...popup.querySelectorAll<HTMLElement>('[role="menuitem"]')];
  expect(popup.getAttribute("aria-label")).toBe("Review actions"); expect(items[1]!.getAttribute("aria-disabled")).toBe("true");
  await interact(() => items[0]!.click()); expect(action).toHaveBeenCalledTimes(1);
 });
 it("opens a named navigation section without losing its crawlable destinations", async () => {
  const r = await render("NavigationMenu"), root = flavor === "react" ? r.b : r.a; expect(root.querySelector('a[href="/guide"]')).toBeTruthy();
  await interact(() => root.querySelector<HTMLButtonElement>("[data-navigation-menu-trigger]")!.click());
  expect(document.querySelector("[data-navigation-menu-surface]")!.textContent).toContain("Guide");
 });
 it("filters the editable combobox and reports selection independently from typed text", async () => {
  const r = await render("Combobox"), root = flavor === "react" ? r.b : r.a, input = root.querySelector("input:not([type=hidden])") as HTMLInputElement;
  await interact(() => input.focus()); await type(input, "Detail"); await interact(() => input.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true, cancelable: true })));
  const popup = document.querySelector<HTMLElement>("[data-combobox-surface]")!, item = popup.querySelector<HTMLElement>('[role="option"]')!;
  expect(item.textContent?.trim()).toBe("Details"); await interact(() => { item.dispatchEvent(new MouseEvent("pointerup", { bubbles: true, cancelable: true })); item.click(); });
  expect(r.changed[flavor].mock.calls.at(-1)?.[0]).toBe("details"); expect(new FormData(root.querySelector("form")!).get("region")).toBe("details");
 });
 it("routes plain in-app links once while preserving native modified and fragment clicks", async () => {
  const navigate = vi.fn(), r = await render("NavigationScope", { navigate }), root = flavor === "react" ? r.b : r.a, link = root.querySelector('a[href="/inspect"]')!;
  const analytics = vi.fn(); document.addEventListener("click", analytics);
  try {
   await interact(() => link.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true, ctrlKey: true })));
   expect(navigate).not.toHaveBeenCalled();
   await interact(() => link.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true })));
   expect(navigate).toHaveBeenCalledTimes(1); expect(navigate.mock.calls[0]![0]).toBe("/inspect"); expect(analytics).toHaveBeenCalledTimes(2);
  } finally { document.removeEventListener("click", analytics); }
 });
});

for (const flavor of ["svelte", "react"] as const) describe(`${flavor} descriptions and range forms`, () => {
 it("keeps a link preview a native link and never steals focus", async () => {
  const r = await render("LinkPreview"), root = flavor === "react" ? r.b : r.a, link = root.querySelector<HTMLAnchorElement>("[data-link-preview-trigger]")!;
  expect(link.getAttribute("href")).toBe("#preview"); expect(link.getAttribute("role")).toBe("link"); link.focus();
  await r.update({ open: true }); expect(document.querySelector("[data-link-preview-surface]")?.getAttribute("aria-label")).toBe("Guide preview"); expect(document.activeElement).toBe(link);
 });
 it("exposes a supplemental tooltip description on keyboard focus", async () => {
  const r = await render("Tooltip"), root = flavor === "react" ? r.b : r.a, trigger = root.querySelector<HTMLElement>("[data-tooltip-trigger]")!;
  await interact(() => { document.dispatchEvent(new KeyboardEvent("keydown", { key: "Tab", bubbles: true })); trigger.focus(); });
  await act(() => new Promise(resolve => setTimeout(resolve, 30)));
  const description = document.getElementById(trigger.getAttribute("aria-describedby")!); expect(description?.textContent).toContain("Review help"); expect(trigger.getAttribute("aria-label")).toBe("Help");
 });
 for (const name of ["DateRangeField", "TimeRangeField"] as const) it(`${name} excludes inverted end values and announces an error`, async () => {
  const r = await render(name, { startValue: name === "DateRangeField" ? "2026-06-12" : "12:30", endValue: name === "DateRangeField" ? "2026-06-10" : "10:00" }), root = flavor === "react" ? r.b : r.a;
  expect(new FormData(root.querySelector("form")!).get("to")).toBeNull(); expect(root.querySelector('[role="alert"]')?.textContent).toContain("cannot be before");
 });
});

for (const flavor of ["svelte", "react"] as const) describe(`${flavor} portal boundaries`, () => {
 it("preserves all three subtree axes and follows live changes", async () => {
  const r = await render("Popover"), root = flavor === "react" ? r.b : r.a;
  for (const [axis, value] of [["theme", "marketing"], ["color-scheme", "light"], ["density", "tiny"]]) root.setAttribute(`data-${axis}`, value!);
  await interact(() => root.querySelector<HTMLButtonElement>("[data-popover-trigger]")!.click()); const popup = document.querySelector<HTMLElement>("[data-popover-surface]")!;
  for (const [axis, value] of [["theme", "marketing"], ["color-scheme", "light"], ["density", "tiny"]]) expect(popup.closest(`[data-${axis}]`)?.getAttribute(`data-${axis}`)).toBe(value);
  await interact(() => root.setAttribute("data-density", "spacious")); expect(popup.closest('[data-density]')?.getAttribute("data-density")).toBe("spacious");
 });
 it("keeps nested popovers in their native modal and dismisses one layer at a time", async () => {
  const r = await render("Dialog"), root = flavor === "react" ? r.b : r.a, dialog = root.querySelector("dialog")!;
  await interact(() => root.querySelector<HTMLButtonElement>("[data-dialog-trigger]")!.click());
  const trigger = dialog.querySelector<HTMLButtonElement>("[data-popover-trigger]")!; await interact(() => trigger.click());
  const popup = document.querySelector<HTMLElement>("[data-popover-surface]")!; expect(popup.closest("dialog")).toBe(dialog); expect(popup.contains(document.activeElement)).toBe(true);
  await interact(() => document.activeElement!.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true })));
  expect(dialog.open).toBe(true); expect(trigger.getAttribute("aria-expanded")).toBe("false");
 });
});

for (const flavor of ["svelte", "react"] as const) for (const name of ["DatePicker", "DateRangePicker"] as const) it(`${flavor} ${name} restores the initial empty form value after a Calendar selection`, async () => {
  const r = await render(name, { name: "date", startName: "start", endName: "end" }), root = flavor === "react" ? r.b : r.a;
  await interact(() => root.querySelector<HTMLButtonElement>("[data-popover-trigger]")!.click());
  const day = (value: string) => document.querySelector<HTMLElement>(`[data-calendar-date="${value}"], [data-bits-day][data-value="${value}"]`)!;
  await interact(() => day("2026-06-10").click()); if (name === "DateRangePicker") await interact(() => day("2026-06-12").click());
  const form = root.querySelector("form")!; expect(new FormData(form).get(name === "DatePicker" ? "date" : "start")).toBe("2026-06-10");
  const calls = r.changed[flavor].mock.calls.length; await interact(() => form.reset()); await act(() => new Promise(resolve => setTimeout(resolve, 20)));
  expect(new FormData(form).get(name === "DatePicker" ? "date" : "start")).toBeNull(); expect(r.changed[flavor]).toHaveBeenCalledTimes(calls);
});

for (const flavor of ["svelte", "react"] as const) describe(`${flavor} locale-aware calendar geometry`, () => {
 for (const weekStartsOn of [0, 1] as const) it(`keeps explicit week start ${weekStartsOn} independent of en-GB locale`, async () => {
  const r = await render("Calendar", { locale: "en-GB", weekStartsOn }), root = flavor === "react" ? r.b : r.a;
  const first = root.querySelector<HTMLElement>("[data-calendar-date], [data-bits-day]")!;
  expect(first.dataset.calendarDate ?? first.dataset.value).toBe(weekStartsOn === 0 ? "2026-05-31" : "2026-06-01");
  const day = root.querySelector<HTMLElement>('[data-calendar-date="2026-06-10"], [data-bits-day][data-value="2026-06-10"]')!; day.focus();
  await interact(() => day.dispatchEvent(new KeyboardEvent("keydown", {key:"Home",bubbles:true,cancelable:true})));
  const active = document.activeElement as HTMLElement; expect(active.dataset.calendarDate ?? active.dataset.value).toBe(weekStartsOn === 0 ? "2026-06-07" : "2026-06-08");
 });
 it("preserves non-Gregorian month labels, date identity and right-to-left direction", async () => {
  const date = toCalendar(parseDate("2026-06-10"), new HebrewCalendar());
  const r = await render("Calendar", { locale: "he-IL", placeholder: date }), root = flavor === "react" ? r.b : r.a;
  expect(root.querySelector('[dir="rtl"]')).toBeTruthy();
  const month = new Intl.DateTimeFormat("he-IL", {month:"long",calendar:"hebrew"}).format(date.toDate("UTC"));
  expect(root.textContent).toContain(month);
  const day = root.querySelector<HTMLElement>('[data-calendar-date="2026-06-10"], [data-bits-day][data-value="2026-06-10"]')!;
  await interact(() => day.click()); expect(r.changed[flavor].mock.calls.at(-1)?.[0].calendar.identifier).toBe("hebrew");
 });
});

for (const flavor of ["svelte","react"] as const) it(`${flavor} keeps boundary Tab inside an anchored focus scope`, async () => {
 const r=await render("Popover"),root=flavor==="react"?r.b:r.a;await interact(()=>root.querySelector<HTMLButtonElement>("[data-popover-trigger]")!.click());
 const popup=document.querySelector<HTMLElement>("[data-popover-surface]")!,button=popup.querySelector("button")!;button.focus();
 const tab=new KeyboardEvent("keydown",{key:"Tab",bubbles:true,cancelable:true});await interact(()=>button.dispatchEvent(tab));expect(tab.defaultPrevented).toBe(true);expect(popup.contains(document.activeElement)).toBe(true);expect(root.querySelector("[data-popover-trigger]")!.getAttribute("aria-expanded")).toBe("true");
});
