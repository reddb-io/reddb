import { flushSync } from "svelte";
import { describe, expect, it, vi } from "vitest";
import SpecializedEntryConsumer from "./fixtures/SpecializedEntryConsumer.svelte";
import SpecializedEntryContractFailures from "./fixtures/SpecializedEntryContractFailures.svelte";
import { FileInput, fileInput } from "./fixtures/specialized-entry-consumer";
import { classes, classesOf, render, rendered } from "./mount";

function filenameFeedbackFailures(root: HTMLElement): string[] {
  const input = root.querySelector<HTMLInputElement>('input[type="file"]');
  const feedback = root.querySelector<HTMLElement>("[data-file-name]");
  return input && feedback?.getAttribute("aria-live") === "polite"
    ? []
    : ["selected filename is not exposed as polite visible feedback"];
}

function setFiles(input: HTMLInputElement, names: string[]): void {
  const files = names.map((name) => new File([name], name, { type: "text/plain" }));
  Object.defineProperty(input, "files", {
    configurable: true,
    writable: true,
    value: Object.assign(files, { item: (index: number) => files[index] ?? null }),
  });
}

describe("the Base FileInput", () => {
  it("is available with its Extension Seam through the Base consumer subpath", () => {
    expect(FileInput).toBeDefined();
    expect(fileInput).toBeTypeOf("function");
  });

  it("composes Field and Input into one labeled native file control", () => {
    const root = rendered(render(FileInput, {
      label: "Upload evidence",
      help: "PDF only.",
      name: "evidence",
      accept: ".pdf",
      multiple: true,
      required: true,
    }));
    const input = root.querySelector<HTMLInputElement>('input[type="file"]')!;

    expect(input.type).toBe("file");
    expect(input.name).toBe("evidence");
    expect(input.accept).toBe(".pdf");
    expect(input.multiple).toBe(true);
    expect(input.required).toBe(true);
    expect(root.querySelector<HTMLLabelElement>(`label[for="${input.id}"]`)?.textContent)
      .toContain("Upload evidence");
    expect(input.getAttribute("aria-describedby")).toContain(`${input.id}-help`);
    input.focus();
    expect(document.activeElement).toBe(input);
  });

  it("keeps selected filenames visible and announced while forwarding change", () => {
    const onchange = vi.fn();
    const root = rendered(render(FileInput, { label: "Attachments", multiple: true, onchange }));
    const input = root.querySelector<HTMLInputElement>('input[type="file"]')!;
    const feedback = root.querySelector<HTMLElement>("[data-file-name]")!;

    expect(feedback.textContent).toContain("No files selected");
    setFiles(input, ["trace.txt", "report.txt"]);
    input.dispatchEvent(new Event("change", { bubbles: true }));
    flushSync();

    expect(feedback.textContent).toContain("trace.txt, report.txt");
    expect(feedback.getAttribute("aria-live")).toBe("polite");
    expect(onchange).toHaveBeenCalledTimes(1);
    expect(filenameFeedbackFailures(root)).toEqual([]);
  });

  it("wears token-backed appearance and inherits a nested appearance scope", () => {
    const root = rendered(render(FileInput, { label: "Upload", class: "min-w-0" }));
    const styles = fileInput();
    const input = root.querySelector<HTMLInputElement>('input[type="file"]')!;
    const nested = rendered(render(SpecializedEntryConsumer, {}))
      .querySelector<HTMLElement>('[data-appearance-scope] [data-file-input]')!;

    expect(classes(root)).toEqual(classesOf(styles.root()));
    expect(classes(input)).toEqual(classesOf(styles.control({ class: "min-w-0" })));
    expect(classes(input).has("h-[var(--reddb-spatial-control-height-md)]")).toBe(true);
    expect(root.hasAttribute("data-theme")).toBe(false);
    expect(root.hasAttribute("data-color-scheme")).toBe(false);
    expect(root.hasAttribute("data-density")).toBe(false);
    expect(nested.hasAttribute("data-theme")).toBe(false);
    expect(nested.hasAttribute("data-color-scheme")).toBe(false);
    expect(nested.hasAttribute("data-density")).toBe(false);
  });
});

describe("the deliberately failing file-input fixture", () => {
  it("diagnoses a file control that hides its selected filename", () => {
    const root = rendered(render(SpecializedEntryContractFailures, {
      failure: "hidden-file-name",
    }));
    expect(filenameFeedbackFailures(root)).toEqual([
      "selected filename is not exposed as polite visible feedback",
    ]);
  });
});
