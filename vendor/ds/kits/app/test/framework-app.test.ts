import { mount, unmount, flushSync } from "svelte";
import { act, createElement as h, type ComponentType } from "react";
import { createRoot, type Root } from "react-dom/client";
import { renderToString } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";
import * as react from "../react/src";
import Fixture from "./fixtures/FrameworkApp.svelte";
import { labelInName, visibleText, accessibleName } from "./label-in-name";
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
HTMLElement.prototype.scrollIntoView ??= () => {};
const cleanups: (() => Promise<void>)[] = [];
afterEach(async () => { while (cleanups.length) await cleanups.pop()!(); });
const destinations = [{ id: "requests", label: "Requests", href: "#requests", badge: 123, status: { label: "Connected", tone: "success" } }, { id: "settings", label: "Settings", href: "#settings" }];
const defaults = {
 NodeBadge: { name: "sao-01", status: "online" }, Pill: {}, ListRow: { title: "Request 204", description: "Completed" }, NavItem: { label: "Requests", href: "#requests", current: true },
 SplitView: {}, ActionPanel: { title: "Retention" }, ActivityFeed: { label: "Recent events", items: [{ id: "completed", title: "Request completed", datetime: "2026-10-20", time: "Now" }] }, ApplicationShell: { embedded: true },
 BottomNavigation: { label: "Workspace", items: destinations, currentId: "requests" }, CardHeading: { title: "Recent requests" }, CategoryFilter: { legend: "Services", name: "service", options: [{ value: "api", label: "API" }, { value: "worker", label: "Worker" }, { value: "off", label: "Offline", disabled: true }] },
 CategoryPreview: { name: "Compute", href: "#compute" }, ChatMessage: { author: "Ana", avatarFallback: "AN", datetime: "2026-10-20", time: "Now" },
 CheckoutForm: { sections: [{ legend: "Billing", fields: [{ name: "company", label: "Company", value: "RedDB", required: true }] }], submitLabel: "Continue" },
 CommandPalette: { triggerLabel: "Commands", contentLabel: "Workspace commands", label: "Find a command", commands: [{ id: "inspect", label: "Inspect request" }] },
 ContextMenu: { triggerLabel: "Request actions", contentLabel: "Request commands", items: [{ id: "inspect", label: "Inspect" }] },
 Diff: { rows: [{ before: "GET /old", after: "GET /new" }] },
 Filter: { legend: "Environment", name: "env", options: [{ value: "prod", label: "Production" }, { value: "stg", label: "Staging" }, { value: "off", label: "Offline", disabled: true }] },
 Incentive: { title: "Faster requests" }, Menubar: { label: "Editor", menus: [{ id: "file", label: "File", items: [{ id: "save", label: "Save" }] }] }, MultiColumnLayout: {},
 OrderHistory: { title: "Invoices", caption: "Recent invoices", orders: [{ id: "INV-42", placed: "Today", placedAt: "2026-10-20", status: "Paid", total: "$42" }] },
 OrderSummary: { title: "Monthly usage", lines: [{ label: "Requests", value: "$42" }], totalLabel: "Total", total: "$42" }, PageHeading: { title: "Requests" },
 ProductFeature: { title: "Request tracing" }, ProductList: { label: "Services", items: [{ id: "api", name: "API", href: "#api", price: "$42" }] }, ProductOverview: { title: "API", price: "$42" },
 ProductQuickview: { title: "API", triggerLabel: "Inspect API" }, Review: { author: "Ana", avatarFallback: "AN", rating: 4 },
 ShoppingCart: { title: "Resources", items: [{ id: "api", name: "API", price: "$42", quantity: 2, quantityName: "quantity" }], totalLabel: "Total", total: "$84" },
 SidebarNavigation: { label: "Workspace", items: destinations, currentId: "requests" }, SidebarPanel: { title: "Requests", badge: 123, status: { label: "Connected", tone: "success" } },
 SidebarRail: { label: "Workspace", items: destinations, currentId: "requests", showLabels: true }, SidebarLayout: { main: false, sidebarLabel: "Workspace" },
 SpeedDial: { triggerLabel: "Create", contentLabel: "Create resource", actions: [{ id: "request", label: "New request" }] },
 StoreNavigation: { label: "Workspace", items: destinations, currentId: "requests" },
 TaskList: { label: "Requests", items: [{ id: "api", title: "Replay requests", state: "running", progress: 40 }, { id: "worker", title: "Rebuild index", state: "blocked", reason: "Waiting for a worker" }] },
 Toolbar: { label: "Request actions", items: [{ id: "inspect", label: "Inspect" }, { id: "guide", label: "Guide", href: "#guide" }, { id: "off", label: "Offline", disabled: true }] },
};
type Name = keyof typeof defaults;
async function interact(fn: () => void) { await act(async () => { fn(); flushSync(); }); }
async function render(name: Name, flavor: "svelte" | "react", extra: Record<string, any> = {}, slotNames: string[] = []) {
 const target = document.createElement("div"); document.body.append(target); let root: Root | undefined, instance: ReturnType<typeof mount<typeof Fixture>> | undefined;
 const native = (extra: Record<string, any>) => Object.fromEntries(Object.entries({ ...defaults[name], ...extra }).map(([key, value]) => [key === "className" ? "class" : key.startsWith("on") ? key.toLowerCase() : key, value]));
 const element = (extra: Record<string, any>) => {
  const props: Record<string, any> = { ...defaults[name], ...extra };
  if (name === "SplitView") Object.assign(props, { start: "Before", end: "After" });
  if (name === "SidebarLayout") Object.assign(props, { sidebar: ({ item, close }: any) => h("div", null, h("button", { onClick: close }, "Close context"), h("a", { href: "#detail" }, `${item?.label ?? "Navigation"} detail`)) });
  if (!["CheckoutForm", "ShoppingCart", "CategoryFilter", "Filter"].includes(name)) props.children = h("span", null, "Request details");
  for (const slot of slotNames) props[slot] = ["TaskList", "ProductQuickview"].includes(name) && slot === "actions" ? (context: any) => h("span", {"data-provided-slot":true}, "Caller detail", context.close ? h("button", {onClick:context.close}, "Close via slot") : undefined) : h("span", {"data-provided-slot":true}, "Caller detail");
  const node = h(react[name] as ComponentType<any>, props);
  return ["CategoryFilter", "Filter"].includes(name) ? h("form", null, node) : node;
 };
 if (flavor === "svelte") { instance = mount(Fixture, { target, props: { name, options: native(extra), slotNames } }); flushSync(); }
 else { root = createRoot(target); await act(() => root!.render(element(extra))); }
 cleanups.push(async () => { if (root) await act(() => root!.unmount()); if (instance) await unmount(instance); target.remove(); });
 return { target, update: async (extra: Record<string, any>) => { if (instance) flushSync(() => instance!.update(native(extra))); else await act(() => root!.render(element(extra))); } };
}
for (const name of Object.keys(defaults) as Name[]) it(`renders ${name} as native React SSR without assigning appearance axes`, () => {
 const html = renderToString(h(react[name] as ComponentType<any>, defaults[name])); expect(html.length).toBeGreaterThan(0); expect(html).not.toMatch(/data-(?:theme|color-scheme|density)=/);
});
for (const flavor of ["svelte", "react"] as const) describe(`${flavor} App contracts`, () => {
 for (const name of Object.keys(defaults) as Name[]) it(`mounts ${name} with its required content and updates without losing the root`, async () => {
  const r = await render(name, flavor), first = r.target.firstElementChild; expect(first).toBeTruthy(); await r.update({ className: "consumer-extension" }); expect(r.target.firstElementChild).toBe(first);
 });
 it("dismisses a Pill once and preserves a native link's destination and current token", async () => {
  const dismiss = vi.fn(), p = await render("Pill", flavor, { onDismiss: dismiss }); await interact(() => p.target.querySelector("button")!.click()); expect(dismiss).toHaveBeenCalledTimes(1);
  const r = await render("NavItem", flavor, { current: "location" }), link = r.target.querySelector("a")!; expect(link.getAttribute("href")).toBe("#requests"); expect(link.getAttribute("aria-current")).toBe("location");
  await r.update({ disabled: true }); expect(r.target.querySelector("a")!.hasAttribute("href")).toBe(false);
 });
 it("serializes and resets category choices silently, then continues from the reset state", async () => {
  const changed = vi.fn(), r = await render("CategoryFilter", flavor, { onValueChange: changed }), form = r.target.querySelector("form")!, boxes = [...form.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')];
  await interact(() => boxes[0]!.click()); await interact(() => boxes[1]!.click()); expect(new FormData(form).getAll("service")).toEqual(["api", "worker"]);
  await interact(() => form.reset()); await act(() => new Promise(resolve => setTimeout(resolve, 20))); expect(changed).toHaveBeenCalledTimes(2); expect(new FormData(form).getAll("service")).toEqual([]);
  await interact(() => boxes[1]!.click()); expect(changed.mock.calls.at(-1)?.[0]).toEqual(["worker"]); expect(boxes[2]!.disabled).toBe(true);
 });
 it("selects one filter and omits disabled choices from activation", async () => {
  const changed = vi.fn(), r = await render("Filter", flavor, { onValueChange: changed }), buttons = [...r.target.querySelectorAll("button")];
  await interact(() => buttons[1]!.click()); expect(new FormData(r.target.querySelector("form")!).get("env")).toBe("stg"); expect(changed).toHaveBeenCalledWith("stg"); expect(buttons[2]!.disabled).toBe(true);
 });
 it("updates external checkout data while allowing edits and preserving the native reset baseline", async () => {
  const r = await render("CheckoutForm", flavor), input = r.target.querySelector("input")!; input.value = "Edited"; expect(new FormData(r.target.querySelector("form")!).get("company")).toBe("Edited");
  await r.update({ sections: [{ legend: "Billing", fields: [{ name: "company", label: "Company", value: "Updated", required: true }] }] }); expect(r.target.querySelector("input")).toBe(input); expect(r.target.querySelector("input")!.value).toBe("Updated");
  expect(r.target.querySelector("label")!.htmlFor).toBe(input.id); await interact(() => r.target.querySelector("form")!.reset()); expect(input.value).toBe("RedDB");
 });
 it("preserves exact notification counts and never turns a status marker into another focus target", async () => {
  for (const name of ["SidebarNavigation", "SidebarRail", "BottomNavigation"] as const) { const r = await render(name, flavor), links = [...r.target.querySelectorAll("a")]; expect(links[0]!.getAttribute("aria-label")).toContain("123"); expect(links[0]!.getAttribute("aria-label")).toContain("Connected"); expect(links[0]!.getAttribute("aria-current")).toBe("page"); expect(labelInName(links[0]!), `${name}: ${visibleText(links[0]!)} / ${accessibleName(links[0]!)}`).toBe(true); expect(links[0]!.querySelectorAll("button, a, [tabindex]")).toHaveLength(0); }
 });
 it("resizes panes by keyboard within bounds and reports the clamped percentage", async () => {
  const changed = vi.fn(), r = await render("SplitView", flavor, { onFractionChange: changed }), divider = r.target.querySelector<HTMLElement>('[role="separator"]')!;
  await interact(() => divider.dispatchEvent(new KeyboardEvent("keydown", { key: "End", bubbles: true, cancelable: true }))); expect(Number(divider.getAttribute("aria-valuenow"))).toBeLessThanOrEqual(90); expect(changed).toHaveBeenCalled();
  await interact(() => divider.dispatchEvent(new KeyboardEvent("keydown", { key: "Home", bubbles: true, cancelable: true }))); expect(Number(divider.getAttribute("aria-valuenow"))).toBeGreaterThanOrEqual(10);
 });
 it("renders task lifecycle, observed progress and a meaningful empty state", async () => {
  const r = await render("TaskList", flavor); expect(r.target.querySelector('[data-state="blocked"]')?.textContent).toContain("Waiting for a worker"); expect(r.target.querySelector('[role="progressbar"]')?.getAttribute("aria-valuenow")).toBe("40");
  await r.update({ items: [], emptyDescription: "Run a request to see its progress" }); expect(r.target.textContent).toContain("No tasks"); expect(r.target.textContent).toContain("Run a request");
 });
 it("keeps exactly one main landmark across nested layout boundaries", async () => {
  const r = await render("SidebarLayout", flavor, { main: true }); expect(r.target.querySelectorAll("main")).toHaveLength(1); expect(r.target.querySelector("main")!.id).toBe("main-content");
 });
 it("opens a native quickview and returns focus to its named trigger", async () => {
  const r = await render("ProductQuickview", flavor), trigger = r.target.querySelector("button")!, dialog = r.target.querySelector("dialog")!; trigger.focus(); await interact(() => trigger.click()); expect(dialog.open).toBe(true);
  await interact(() => dialog.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }))); expect(dialog.open).toBe(false); expect(document.activeElement).toBe(trigger);
 });
 it("keeps context-menu commands accessible from Shift+F10 and suppresses disabled commands", async () => {
  const action = vi.fn(), r = await render("ContextMenu", flavor, { items: [{ id: "inspect", label: "Inspect", onselect: action }, { id: "off", label: "Offline", disabled: true }] }), trigger = r.target.querySelector("button")!; trigger.focus();
  await interact(() => trigger.dispatchEvent(new KeyboardEvent("keydown", { key: "F10", shiftKey: true, bubbles: true, cancelable: true })));
  const menu = document.querySelector('[data-context-menu-surface]')!, items = menu.querySelectorAll<HTMLElement>('[role="menuitem"]'); expect(items[1]!.getAttribute("aria-disabled")).toBe("true"); await interact(() => items[0]!.click()); expect(action).toHaveBeenCalledTimes(1);
 });
});

const slotCases: Partial<Record<Name, string[]>> = {
 ActionPanel:["actions"],ApplicationShell:["header","footer"],CardHeading:["actions"],CategoryPreview:["media"],ChatMessage:["actions"],CheckoutForm:["actions"],Incentive:["icon","actions"],ListRow:["leading","trailing"],MultiColumnLayout:["start","end"],NavItem:["icon","trailing"],OrderSummary:["actions"],PageHeading:["context","actions"],ProductFeature:["media","actions"],ProductOverview:["media","actions"],Review:["actions"],ShoppingCart:["actions"],SidebarNavigation:["toolbar"],SidebarPanel:["toolbar","actions","controls","footer"],SidebarRail:["top","bottom"],StoreNavigation:["brand","actions","announcement"],TaskList:["actions"],
};
for (const flavor of ["svelte","react"] as const) for (const [name, slots] of Object.entries(slotCases) as [Name,string[]][]) it(`${flavor} ${name} keeps caller content in every named seam`, async () => {
 const r=await render(name,flavor,{},slots); expect(r.target.querySelectorAll("[data-provided-slot]").length).toBeGreaterThanOrEqual(slots.length);
});

for (const flavor of ["svelte","react"] as const) describe(`${flavor} optional App content and navigation`,()=>{
 it("retains custom empty states for products, carts and tasks",async()=>{
  for(const name of ["ProductList","ShoppingCart","TaskList"] as const){const r=await render(name,flavor,{items:[]},["empty"]);expect(r.target.querySelector("[data-provided-slot]")?.textContent).toBe("Caller detail");}
 });
 it("lets a quickview action receive the same close operation while replacing its trigger content",async()=>{
  const r=await render("ProductQuickview",flavor,{},["trigger","media","actions"]),trigger=r.target.querySelector("button")!;
  expect(trigger.getAttribute("aria-label")).toBe("Inspect API");await interact(()=>trigger.click());expect(r.target.querySelectorAll("[data-provided-slot]")).toHaveLength(3);
  await interact(()=>[...r.target.querySelectorAll("button")].find(button=>button.textContent==="Close via slot")!.click());expect(r.target.querySelector("dialog")!.open).toBe(false);
 });
 it("keeps a custom speed-dial trigger inside its named button and activates only enabled actions",async()=>{
  const action=vi.fn(),r=await render("SpeedDial",flavor,{actions:[{id:"create",label:"New request",onselect:action},{id:"locked",label:"Unavailable",disabled:true}]},["trigger"]),trigger=r.target.querySelector("button")!;
  expect(trigger.querySelector("[data-provided-slot]")).toBeTruthy();await interact(()=>trigger.click());const surface=document.querySelector("[data-popover-surface]")!;
  expect([...surface.querySelectorAll("button")].find(button=>button.textContent?.trim()==="Unavailable")!.disabled).toBe(true);await interact(()=>[...surface.querySelectorAll("button")].find(button=>button.textContent?.trim()==="New request")!.click());expect(action).toHaveBeenCalledTimes(1);
 });
 it("retains an explicit legacy rail while removing closed contextual content from keyboard navigation",async()=>{
  const r=await render("SidebarLayout",flavor,{panelOpen:false},["rail"]);expect(r.target.querySelector("[data-sidebar-layout-region=rail] [data-provided-slot]")).toBeTruthy();expect(r.target.querySelector("aside")!.hasAttribute("inert") || (r.target.querySelector("aside") as HTMLElement & {inert?:boolean}).inert === true).toBe(true);
 });
 it("keeps primary-navigation identity caller-owned while activating a contextual section",async()=>{
  const action=vi.fn(),r=await render("SidebarLayout",flavor,{navigation:[{id:"requests",label:"Requests",fallback:"RQ",onselect:action},{id:"settings",label:"Settings",href:"#settings"}],currentId:"settings"},["header"]);
  expect(r.target.querySelector("[data-primary-navigation-header] [data-provided-slot]")).toBeTruthy();await interact(()=>r.target.querySelector<HTMLButtonElement>('[data-sidebar-rail-item="requests"]')!.click());expect(action).toHaveBeenCalledTimes(1);expect(r.target.querySelector('[data-sidebar-rail-item="settings"]')!.getAttribute("aria-current")).toBe("page");expect(r.target.querySelector("[data-primary-navigation-panel]")?.textContent).toContain("Requests detail");
 });
 it("activates menubar commands once and keeps disabled entries unavailable",async()=>{
  const action=vi.fn(),r=await render("Menubar",flavor,{menus:[{id:"file",label:"File",items:[{id:"save",label:"Save",onselect:action},{id:"off",label:"Offline",disabled:true}]}]});const trigger=r.target.querySelector("button")!;trigger.focus();await interact(()=>trigger.dispatchEvent(new KeyboardEvent("keydown",{key:"Enter",bubbles:true,cancelable:true})));if(flavor==="react")await interact(()=>trigger.click());const menu=document.querySelector("[data-menubar-surface]")!,items=menu.querySelectorAll<HTMLElement>('[role="menuitem"]');expect(items[1]!.getAttribute("aria-disabled")).toBe("true");await interact(()=>items[0]!.click());expect(action).toHaveBeenCalledTimes(1);
 });
});
