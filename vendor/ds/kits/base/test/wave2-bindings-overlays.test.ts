import { flushSync } from "svelte";
import { describe, expect, it, vi } from "vitest";
import BindingConsumer from "./fixtures/BindingConsumer.svelte";
import ControlledDialogConsumer from "./fixtures/ControlledDialogConsumer.svelte";
import TooltipChildConsumer from "./fixtures/TooltipChildConsumer.svelte";
import UncontrolledFieldConsumer from "./fixtures/UncontrolledFieldConsumer.svelte";
import { Dialog } from "./fixtures/dialog-tooltip-consumer";
import { render } from "./mount";

function type(input: HTMLInputElement | HTMLTextAreaElement, value: string): void {
  input.value = value;
  input.dispatchEvent(new Event("input", { bubbles: true }));
  flushSync();
}

function toggle(input: HTMLInputElement): void {
  input.click();
  flushSync();
}

describe("native form controls bind like the elements they wrap", () => {
  it("propagates bind:value from Input and Textarea to the parent", () => {
    const root = render(BindingConsumer);
    type(root.querySelector<HTMLInputElement>("[data-bound-input]")!, "typed");
    type(root.querySelector<HTMLTextAreaElement>("[data-bound-textarea]")!, "second");
    expect(root.querySelector("[data-text]")!.textContent).toBe("typed");
    expect(root.querySelector("[data-notes]")!.textContent).toBe("second");
  });

  it("propagates bind:checked from Checkbox and Switch to the parent", () => {
    const root = render(BindingConsumer);
    toggle(root.querySelector<HTMLInputElement>("[data-bound-checkbox]")!);
    toggle(root.querySelector<HTMLInputElement>("[data-bound-switch]")!);
    expect(root.querySelector("[data-accepted]")!.textContent).toBe("true");
    expect(root.querySelector("[data-enabled]")!.textContent).toBe("false");
  });

  it("writes parent changes back into the controls", () => {
    const root = render(BindingConsumer);
    root.querySelector<HTMLButtonElement>("[data-reset]")!.click();
    flushSync();
    expect(root.querySelector<HTMLInputElement>("[data-bound-input]")!.value).toBe("reset");
    expect(root.querySelector<HTMLInputElement>("[data-bound-checkbox]")!.checked).toBe(true);
  });
});

describe("uncontrolled controls keep what the person typed", () => {
  it("survives the surrounding Field re-rendering its validation state", () => {
    const root = render(UncontrolledFieldConsumer);
    const input = root.querySelector<HTMLInputElement>("[data-uncontrolled-input]")!;
    const checkbox = root.querySelector<HTMLInputElement>("[data-uncontrolled-checkbox]")!;
    type(input, "maya@example.com");
    toggle(checkbox);

    for (let pass = 0; pass < 2; pass += 1) {
      root.querySelector<HTMLButtonElement>("[data-validate]")!.click();
      flushSync();
      expect(input.value).toBe("maya@example.com");
      expect(checkbox.checked).toBe(true);
    }
  });
});

describe("a Dialog driven through bind:open", () => {
  it("renders no DS trigger when triggerLabel is omitted and opens from outside", () => {
    const root = render(ControlledDialogConsumer);
    const dialog = root.querySelector<HTMLDialogElement>("[data-settings-dialog]")!;
    expect(root.querySelector("[data-dialog-trigger]")).toBeNull();
    expect(dialog.open).toBe(false);

    root.querySelector<HTMLButtonElement>("[data-open-settings]")!.click();
    flushSync();
    expect(dialog.open).toBe(true);
  });

  it("writes every close path back to the bound state and reports it", () => {
    const root = render(ControlledDialogConsumer);
    const dialog = root.querySelector<HTMLDialogElement>("[data-settings-dialog]")!;
    root.querySelector<HTMLButtonElement>("[data-open-settings]")!.click();
    flushSync();

    dialog.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    flushSync();
    expect(dialog.open).toBe(false);
    expect(root.querySelector("[data-settings-open]")!.textContent).toBe("false");
    expect(root.querySelector("[data-changes]")!.textContent).toBe("false");
  });

  it("locks page scroll while open and restores it on close", () => {
    const root = render(ControlledDialogConsumer);
    const before = document.documentElement.style.overflow;
    root.querySelector<HTMLButtonElement>("[data-open-settings]")!.click();
    flushSync();
    expect(document.documentElement.style.overflow).toBe("hidden");

    root.querySelector<HTMLButtonElement>("[data-settings-dialog] [data-dialog-close]")!.click();
    flushSync();
    expect(document.documentElement.style.overflow).toBe(before);
  });

  it("dismisses on a click outside the surface but not inside it", () => {
    const root = render(Dialog, { triggerLabel: "Open", title: "Settings" });
    root.querySelector<HTMLButtonElement>("[data-dialog-trigger]")!.click();
    const dialog = root.querySelector<HTMLDialogElement>("dialog")!;
    dialog.getBoundingClientRect = () =>
      ({ left: 100, right: 400, top: 100, bottom: 300 }) as DOMRect;

    dialog.dispatchEvent(new MouseEvent("click", { bubbles: true, clientX: 200, clientY: 200 }));
    expect(dialog.open).toBe(true);
    dialog.dispatchEvent(new MouseEvent("click", { bubbles: true, clientX: 20, clientY: 20 }));
    expect(dialog.open).toBe(false);
  });

  it("derives SSR-stable ids per instance instead of a module counter", () => {
    const first = render(Dialog, { triggerLabel: "One", title: "One", description: "a" });
    const second = render(Dialog, { triggerLabel: "Two", title: "Two", description: "b" });
    const ids = [first, second].map((root) => root.querySelector("dialog")!.getAttribute("aria-labelledby"));
    expect(ids[0]).not.toBe(ids[1]);
    expect(ids[0]).not.toMatch(/^reddb-dialog-\d+/);
  });
});

describe("an AlertDialog with an asynchronous confirmation", () => {
  it("stays open and busy until the promise fulfils, then closes", async () => {
    let resolve!: () => void;
    const onconfirm = vi.fn(() => new Promise<void>((done) => (resolve = done)));
    const root = render(ControlledDialogConsumer, { onconfirm });
    root.querySelector<HTMLButtonElement>("[data-open-confirm]")!.click();
    flushSync();
    const dialog = root.querySelector<HTMLDialogElement>('dialog[role="alertdialog"]')!;
    const confirm = dialog.querySelector<HTMLButtonElement>("[data-alert-dialog-confirm]")!;

    confirm.click();
    flushSync();
    expect(dialog.open).toBe(true);
    expect(confirm.getAttribute("aria-busy")).toBe("true");

    resolve();
    await Promise.resolve();
    await Promise.resolve();
    flushSync();
    expect(dialog.open).toBe(false);
  });

  it("stays open when the confirmation rejects", async () => {
    const onconfirm = vi.fn(() => Promise.reject(new Error("forbidden")));
    const root = render(ControlledDialogConsumer, { onconfirm });
    root.querySelector<HTMLButtonElement>("[data-open-confirm]")!.click();
    flushSync();
    const dialog = root.querySelector<HTMLDialogElement>('dialog[role="alertdialog"]')!;
    dialog.querySelector<HTMLButtonElement>("[data-alert-dialog-confirm]")!.click();
    await Promise.resolve();
    await Promise.resolve();
    flushSync();
    expect(dialog.open).toBe(true);
  });

  it("marks the confirmation as the destructive intent, not the Brand accent", () => {
    const root = render(ControlledDialogConsumer);
    const confirm = root.querySelector<HTMLButtonElement>("[data-alert-dialog-confirm]")!;
    expect(confirm.className).toContain("feedback-danger");
    expect(confirm.className).not.toMatch(/(?:^|\s)bg-primary(?:\s|$)/);
  });
});

describe("a Tooltip on a trigger the consumer owns", () => {
  it("decorates the consumer's element instead of rendering its own button", () => {
    const root = render(TooltipChildConsumer);
    const own = root.querySelector<HTMLAnchorElement>("[data-own-trigger]")!;
    expect(own).not.toBeNull();
    expect(own.hasAttribute("data-tooltip-trigger")).toBe(true);
    expect(root.querySelector("button[data-tooltip-trigger]")).toBeNull();
  });
});
