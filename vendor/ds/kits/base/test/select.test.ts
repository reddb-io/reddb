import { flushSync } from "svelte";
import { describe, expect, it, vi } from "vitest";
import TextareaSelectContractFailures from "./fixtures/TextareaSelectContractFailures.svelte";
import TextareaSelectConsumer from "./fixtures/TextareaSelectConsumer.svelte";
import { Select, select } from "./fixtures/textarea-select-consumer";
import { classes, classesOf, render, rendered } from "./mount";

function renderedSelect(props: Record<string, unknown> = {}): HTMLSelectElement {
  return rendered(render(Select, props)) as HTMLSelectElement;
}

function keyboardSelectFailures(root: HTMLElement): string[] {
  const control = root.querySelector("select");
  return control && control.tabIndex >= 0 ? [] : ["control is not a native keyboard select"];
}

describe("the Base Select", () => {
  it("is available with its Extension Seam through the Base consumer subpath", () => {
    expect(Select).toBeDefined();
    expect(select).toBeTypeOf("function");
  });

  it("is the native select and delegates keyboard and focus behavior to the platform", () => {
    const onkeydown = vi.fn();
    const element = renderedSelect({ name: "region", onkeydown });
    expect(element.tagName).toBe("SELECT");
    element.focus();
    expect(document.activeElement).toBe(element);

    element.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
    flushSync();
    expect(onkeydown).toHaveBeenCalledTimes(1);
    expect(onkeydown.mock.calls[0]?.[0]).toMatchObject({ key: "ArrowDown" });
  });

  it("forwards native identity, selection, and constraint attributes unchanged", () => {
    const element = renderedSelect({
      id: "deployment-region",
      name: "region",
      autocomplete: "country-name",
      required: true,
      multiple: true,
      size: 4,
      "aria-invalid": "true",
    });

    expect(element.id).toBe("deployment-region");
    expect(element.name).toBe("region");
    expect(element.getAttribute("autocomplete")).toBe("country-name");
    expect(element.required).toBe(true);
    expect(element.multiple).toBe(true);
    expect(element.size).toBe(4);
    expect(element.getAttribute("aria-invalid")).toBe("true");
  });

  it("renders caller-owned options and participates in native validation and form submission", () => {
    const form = rendered(render(TextareaSelectConsumer, { required: true }));
    const element = form.querySelector<HTMLSelectElement>('select[name="region"]')!;

    expect([...element.options].map(({ value }) => value)).toEqual([
      "",
      "us-east-1",
      "sa-east-1",
    ]);
    expect(element.value).toBe("sa-east-1");
    expect(element.checkValidity()).toBe(true);
    expect(new FormData(form as HTMLFormElement).get("region")).toBe("sa-east-1");

    element.value = "";
    expect(element.checkValidity()).toBe(false);
  });

  it("renders declarative flat options without replacing the native select", () => {
    const element = renderedSelect({
      value: "POST",
      options: [
        { value: "GET", label: "GET" },
        { value: "POST", label: "POST" },
        { value: "TRACE", label: "TRACE", disabled: true },
      ],
    });

    expect([...element.options].map(({ value, text, disabled }) => ({ value, text, disabled })))
      .toEqual([
        { value: "GET", text: "GET", disabled: false },
        { value: "POST", text: "POST", disabled: false },
        { value: "TRACE", text: "TRACE", disabled: true },
      ]);
    expect(element.value).toBe("POST");
  });

  it("forwards native change handlers", () => {
    const onchange = vi.fn();
    const element = renderedSelect({ onchange });
    const option = document.createElement("option");
    option.value = "compact";
    element.append(option);
    element.value = "compact";
    element.dispatchEvent(new Event("change", { bubbles: true }));
    flushSync();
    expect(onchange).toHaveBeenCalledTimes(1);
  });

  it("composes with Field without losing its accessible associations", () => {
    const form = rendered(render(TextareaSelectConsumer, { required: true }));
    const element = form.querySelector<HTMLSelectElement>('select[name="region"]')!;
    const label = form.querySelector<HTMLLabelElement>(`label[for="${element.id}"]`);
    expect(label?.textContent).toContain("Region");
    expect(element.required).toBe(true);
  });

  it("wears exactly its exported appearance and merges consumer classes", () => {
    const element = renderedSelect({ class: "min-w-0" });
    expect(classes(element)).toEqual(classesOf(select({ class: "min-w-0" })));
    expect(classes(element).has("min-w-0")).toBe(true);
    expect(classes(element).has("focus-visible:outline-focus")).toBe(true);
  });
});

describe("the deliberately failing keyboard-select fixture", () => {
  it("diagnoses a pointer-only substitute", () => {
    const root = rendered(render(TextareaSelectContractFailures, { failure: "mouse-only-select" }));
    expect(keyboardSelectFailures(root)).toEqual(["control is not a native keyboard select"]);
  });
});
