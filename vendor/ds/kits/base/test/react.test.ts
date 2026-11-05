import { act, createElement as h, createRef, useState } from "react";
import { createRoot, hydrateRoot, type Root } from "react-dom/client";
import { renderToString } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Button, Field, Input, Textarea, Heading, Text, Stack, Badge, Divider, Container, LoadingIndicator } from "../react/src";
import { button } from "../src/button.variants";
import { input } from "../src/input.variants";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
let root: Root | undefined;
afterEach(async () => {
  if (root) await act(() => root?.unmount());
  root = undefined;
  document.body.replaceChildren();
});
async function mount(node: ReturnType<typeof h>) {
  const container = document.createElement("div");
  document.body.append(container);
  await act(() => { root = createRoot(container); root.render(node); });
  return container;
}

describe("native React implementation", () => {
  it("uses the canonical recipes, keeps native refs and defaults to a non-submitting button", async () => {
    const ref = createRef<HTMLButtonElement>();
    const container = await mount(h(Button, { ref, children: "Save" }));
    const element = container.querySelector("button")!;
    expect(ref.current).toBe(element);
    expect(element.type).toBe("button");
    expect(element.className).toBe(button());
    expect(renderToString(h(Input, { "aria-label": "Name" }))).toContain(input());
  });

  it("removes link activation and announces loading without losing its label", async () => {
    const click = vi.fn();
    const container = await mount(h(Button, { href: "/details", loading: true, onClick: click, children: "Details" }));
    const element = container.querySelector("a")!;
    element.click();
    expect(click).not.toHaveBeenCalled();
    expect(element.hasAttribute("href")).toBe(false);
    expect(element.tabIndex).toBe(-1);
    expect(element.getAttribute("aria-busy")).toBe("true");
    expect(element.getAttribute("aria-disabled")).toBe("true");
    expect(element.textContent).toBe("Details");
  });

  it("hydrates generated Field associations without changing ids", async () => {
    const field = h(Field, { label: "Organization", help: "Used for billing", error: "Choose a name", required: true,
      children: control => h(Input, { ...control, name: "organization", defaultValue: "RedDB" }) });
    const container = document.createElement("div");
    container.innerHTML = renderToString(field);
    document.body.append(container);
    const serverId = container.querySelector("input")!.id;
    const errors: unknown[] = [];
    await act(() => { root = hydrateRoot(container, field, { onRecoverableError: error => errors.push(error) }); });
    const element = container.querySelector("input")!;
    expect(element.id).toBe(serverId);
    expect(container.querySelector("label")!.htmlFor).toBe(serverId);
    expect(element.required).toBe(true);
    expect(element.getAttribute("aria-invalid")).toBe("true");
    const descriptions = element.getAttribute("aria-describedby")!.split(" ");
    expect(descriptions).toEqual([`${serverId}-help`, `${serverId}-error`]);
    expect(errors).toEqual([]);
  });

  it("retains native form serialization and reset for uncontrolled controls", async () => {
    const container = await mount(h("form", null,
      h(Input, { name: "name", defaultValue: "Initial", "aria-label": "Name" }),
      h(Textarea, { name: "notes", defaultValue: "Original", "aria-label": "Notes" })));
    const form = container.querySelector("form")!;
    form.querySelector("input")!.value = "Edited";
    form.querySelector("textarea")!.value = "Changed";
    expect(new FormData(form).get("name")).toBe("Edited");
    await act(() => form.reset());
    expect(new FormData(form).get("name")).toBe("Initial");
    expect(new FormData(form).get("notes")).toBe("Original");
  });

  it("composes controlled values and preserves the native React event", async () => {
    const event = vi.fn();
    const changed = vi.fn();
    function Form() {
      const [value, setValue] = useState("Original");
      return h(Input, { value, "aria-label": "Name", onChange: event, onValueChange: next => { changed(next); setValue(next); } });
    }
    const container = await mount(h(Form));
    const element = container.querySelector("input")!;
    // Use the native setter so React observes a user edit rather than a tracked programmatic assignment.
    await act(() => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(element, "Changed");
      element.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expect(element.value).toBe("Changed");
    expect(changed).toHaveBeenCalledWith("Changed");
    expect(event.mock.calls[0]?.[0].type).toBe("change");
  });

  it("renders meaningful static HTML without selecting an appearance axis", () => {
    const html = renderToString(h(Container, null, h(Stack, null,
      h(Heading, { level: 1 }, "Operations"), h(Text, { role: "caption" }, "Current state"),
      h(Badge, { tone: "success", children: "Ready" }), h(Divider, { orientation: "vertical" }),
      h(LoadingIndicator, { label: "Refreshing requests", labelHidden: true }))));
    expect(html).toContain("<h1");
    expect(html).toContain('aria-orientation="vertical"');
    expect(html).toContain('role="status"');
    expect(html).toContain("Refreshing requests");
    expect(html).toContain("motion-reduce:");
    expect(html).not.toMatch(/data-(?:theme|color-scheme|density)=/);
  });
});
