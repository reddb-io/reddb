import { flushSync } from "svelte";
import { describe, expect, it, vi } from "vitest";
import TextareaSelectContractFailures from "./fixtures/TextareaSelectContractFailures.svelte";
import TextareaSelectConsumer from "./fixtures/TextareaSelectConsumer.svelte";
import { Textarea, textarea } from "./fixtures/textarea-select-consumer";
import { classes, classesOf, render, rendered } from "./mount";

function renderedTextarea(props: Record<string, unknown> = {}): HTMLTextAreaElement {
  return rendered(render(Textarea, props)) as HTMLTextAreaElement;
}

function multilineFailures(root: HTMLElement): string[] {
  return root.querySelector("textarea") ? [] : ["control is not multiline"];
}

describe("the Base Textarea", () => {
  it("is available with its Extension Seam through the Base consumer subpath", () => {
    expect(Textarea).toBeDefined();
    expect(textarea).toBeTypeOf("function");
  });

  it("is the native multiline control and keeps native focus behavior", () => {
    const element = renderedTextarea({ name: "notes", rows: 5 });
    expect(element.tagName).toBe("TEXTAREA");
    expect(element.rows).toBe(5);
    element.focus();
    expect(document.activeElement).toBe(element);
  });

  it("forwards native entry, sizing, and constraint attributes unchanged", () => {
    const element = renderedTextarea({
      id: "deployment-notes",
      name: "notes",
      autocomplete: "off",
      placeholder: "Describe the rollback plan",
      required: true,
      minlength: 10,
      maxlength: 200,
      rows: 6,
      cols: 48,
      wrap: "hard",
      "aria-invalid": "true",
      value: "Rollback after validation.",
    });

    expect(element.id).toBe("deployment-notes");
    expect(element.name).toBe("notes");
    expect(element.autocomplete).toBe("off");
    expect(element.placeholder).toBe("Describe the rollback plan");
    expect(element.required).toBe(true);
    expect(element.minLength).toBe(10);
    expect(element.maxLength).toBe(200);
    expect(element.rows).toBe(6);
    expect(element.cols).toBe(48);
    expect(element.wrap).toBe("hard");
    expect(element.getAttribute("aria-invalid")).toBe("true");
    expect(element.value).toBe("Rollback after validation.");
  });

  it("participates in native validation and form submission", () => {
    const element = renderedTextarea({ name: "notes", required: true });
    const form = document.createElement("form");
    form.append(element);
    expect(element.checkValidity()).toBe(false);

    element.value = "Ready to deploy.";
    expect(element.checkValidity()).toBe(true);
    expect(new FormData(form).get("notes")).toBe("Ready to deploy.");
  });

  it("forwards native input handlers", () => {
    const oninput = vi.fn();
    const element = renderedTextarea({ oninput });
    element.value = "A second line";
    element.dispatchEvent(new InputEvent("input", { bubbles: true }));
    flushSync();
    expect(oninput).toHaveBeenCalledTimes(1);
  });

  it("composes with Field without losing its accessible associations", () => {
    const form = rendered(
      render(TextareaSelectConsumer, { error: "Add rollback details.", required: true }),
    );
    const element = form.querySelector<HTMLTextAreaElement>('textarea[name="notes"]')!;
    const label = form.querySelector<HTMLLabelElement>(`label[for="${element.id}"]`);
    expect(label?.textContent).toContain("Deployment notes");
    expect(element.getAttribute("aria-describedby")).toContain(`${element.id}-help`);
    expect(element.getAttribute("aria-describedby")).toContain(`${element.id}-error`);
    expect(element.getAttribute("aria-errormessage")).toBe(`${element.id}-error`);
    expect(element.required).toBe(true);
  });

  it("wears exactly its exported appearance and merges consumer classes", () => {
    const element = renderedTextarea({ class: "min-w-0" });
    expect(classes(element)).toEqual(classesOf(textarea({ class: "min-w-0" })));
    expect(classes(element).has("min-h-[var(--reddb-spatial-control-height-lg)]")).toBe(true);
    expect(classes(element).has("focus-visible:outline-focus")).toBe(true);
  });
});

describe("the deliberately failing multiline fixture", () => {
  it("diagnoses a single-line substitute", () => {
    const root = rendered(render(TextareaSelectContractFailures, { failure: "single-line" }));
    expect(multilineFailures(root)).toEqual(["control is not multiline"]);
  });
});
