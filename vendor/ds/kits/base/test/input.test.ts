import { flushSync } from "svelte";
import { describe, expect, it, vi } from "vitest";
import { Input, input } from "./fixtures/field-input-consumer";
import { classes, classesOf, render, rendered } from "./mount";

function renderedInput(props: Record<string, unknown> = {}): HTMLInputElement {
  return rendered(render(Input, props)) as HTMLInputElement;
}

describe("the Base Input", () => {
  it("is available with its Extension Seam through the Base consumer subpath", () => {
    expect(Input).toBeDefined();
    expect(input).toBeTypeOf("function");
  });

  it("is the native input and keeps native focus behavior", () => {
    const element = renderedInput({ name: "query", type: "search" });
    expect(element.tagName).toBe("INPUT");
    element.focus();
    expect(document.activeElement).toBe(element);
  });

  it("forwards native identity, entry, and constraint attributes unchanged", () => {
    const element = renderedInput({
      id: "account-email",
      name: "email",
      type: "email",
      autocomplete: "email",
      inputmode: "email",
      placeholder: "name@example.com",
      required: true,
      pattern: ".+@example\\.com",
      maxlength: 64,
      size: 40,
      "aria-invalid": "true",
    });

    expect(element.id).toBe("account-email");
    expect(element.name).toBe("email");
    expect(element.type).toBe("email");
    expect(element.autocomplete).toBe("email");
    expect(element.inputMode).toBe("email");
    expect(element.placeholder).toBe("name@example.com");
    expect(element.required).toBe(true);
    expect(element.pattern).toBe(".+@example\\.com");
    expect(element.maxLength).toBe(64);
    expect(element.size).toBe(40);
    expect(element.getAttribute("aria-invalid")).toBe("true");
  });

  it("retains the browser's native validity contract", () => {
    const element = renderedInput({
      name: "email",
      type: "email",
      required: true,
      pattern: ".+@example\\.com",
      value: "not-an-email",
    });
    expect(element.checkValidity()).toBe(false);

    element.value = "person@example.com";
    expect(element.checkValidity()).toBe(true);
  });

  it("forwards native event handlers", () => {
    const oninput = vi.fn();
    const element = renderedInput({ oninput });
    element.value = "typed";
    element.dispatchEvent(new InputEvent("input", { bubbles: true }));
    flushSync();
    expect(oninput).toHaveBeenCalledTimes(1);
  });

  it("wears exactly its exported appearance and merges consumer classes", () => {
    const element = renderedInput({ class: "min-w-0" });
    expect(classes(element)).toEqual(classesOf(input({ class: "min-w-0" })));
    expect(classes(element).has("min-w-0")).toBe(true);
    // Focus is ink (ADR 0023): the focus outline, never the Brand red.
    expect(classes(element).has("focus-visible:outline-focus")).toBe(true);
    expect([...classes(element)].some((name) => name.includes("ring-primary"))).toBe(false);
  });
});
