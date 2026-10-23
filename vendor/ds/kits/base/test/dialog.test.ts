import { createRawSnippet, type Snippet } from "svelte";
import { describe, expect, it } from "vitest";
import { Dialog, dialog as dialogAppearance } from "./fixtures/dialog-tooltip-consumer";
import { classes, classesOf, render } from "./mount";

function text(content: string): Snippet {
  return createRawSnippet(() => ({ render: () => `<span>${content}</span>` }));
}

describe("the Base Dialog", () => {
  it("replaces the default close control with a custom header while retaining its accessible title", () => {
    const root = render(Dialog, { title: "Contact information", triggerLabel: "Open contact", header: text("Custom contact toolbar"), children: text("Contact details") });
    root.querySelector<HTMLButtonElement>("[data-dialog-trigger]")!.click();
    const dialog = root.querySelector<HTMLDialogElement>("dialog")!;
    const title = dialog.querySelector("[data-dialog-title]")!;
    expect(title.className).toContain("sr-only");
    expect(dialog.getAttribute("aria-labelledby")).toBe(title.id);
    expect(dialog.querySelector("[data-dialog-header]")?.textContent).toBe("Custom contact toolbar");
    expect(dialog.querySelector("[data-dialog-close]")).toBeNull();
    expect(dialog.querySelector("[data-dialog-body]")?.className).toContain("overflow-y-auto");
  });
  it("labels the modal from its visible title and description", () => {
    const root = render(Dialog, {
      triggerLabel: "Edit deployment",
      title: "Edit deployment",
      description: "Changes take effect immediately.",
      children: text("Dialog body"),
    });
    const trigger = root.querySelector<HTMLButtonElement>("[data-dialog-trigger]")!;

    trigger.click();

    const dialog = root.querySelector<HTMLDialogElement>("dialog")!;
    const title = root.querySelector<HTMLElement>("[data-dialog-title]")!;
    const description = root.querySelector<HTMLElement>("[data-dialog-description]")!;
    expect(dialog.open).toBe(true);
    expect(dialog.getAttribute("aria-modal")).toBe("true");
    expect(dialog.getAttribute("aria-labelledby")).toBe(title.id);
    expect(dialog.getAttribute("aria-describedby")).toBe(description.id);
    expect(title.textContent).toBe("Edit deployment");
  });

  it("dismisses from the keyboard with Escape", () => {
    const root = render(Dialog, {
      triggerLabel: "Open settings",
      title: "Settings",
    });
    root.querySelector<HTMLButtonElement>("[data-dialog-trigger]")!.click();
    const dialog = root.querySelector<HTMLDialogElement>("dialog")!;

    dialog.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));

    expect(dialog.open).toBe(false);
  });

  it("moves focus into the modal and restores it after dismissal", () => {
    const root = render(Dialog, {
      triggerLabel: "Open members",
      title: "Members",
      children: createRawSnippet(() => ({
        render: () => '<button data-first-member type="button">Invite member</button>',
      })),
    });
    const trigger = root.querySelector<HTMLButtonElement>("[data-dialog-trigger]")!;
    trigger.focus();

    trigger.click();
    const dialog = root.querySelector<HTMLDialogElement>("dialog")!;
    expect(document.activeElement).toBe(root.querySelector("[data-first-member]"));

    dialog.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(document.activeElement).toBe(trigger);
  });

  it("contains Tab navigation within the modal", () => {
    const root = render(Dialog, {
      triggerLabel: "Open actions",
      title: "Actions",
      children: createRawSnippet(() => ({
        render: () =>
          '<div><button data-first type="button">First</button><button data-last type="button">Last</button></div>',
      })),
    });
    root.querySelector<HTMLButtonElement>("[data-dialog-trigger]")!.click();
    const dialog = root.querySelector<HTMLDialogElement>("dialog")!;
    const first = root.querySelector<HTMLButtonElement>("[data-dialog-close]")!;
    const last = root.querySelector<HTMLButtonElement>("[data-last]")!;

    last.focus();
    dialog.dispatchEvent(new KeyboardEvent("keydown", { key: "Tab", bubbles: true }));
    expect(document.activeElement).toBe(first);

    first.focus();
    dialog.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Tab", shiftKey: true, bubbles: true }),
    );
    expect(document.activeElement).toBe(last);
  });

  it("provides a labeled close control", () => {
    const root = render(Dialog, { triggerLabel: "Open help", title: "Help" });
    root.querySelector<HTMLButtonElement>("[data-dialog-trigger]")!.click();
    const dialog = root.querySelector<HTMLDialogElement>("dialog")!;
    const close = root.querySelector<HTMLButtonElement>("[data-dialog-close]")!;

    expect(close.getAttribute("aria-label")).toBe("Close Help");
    close.click();
    expect(dialog.open).toBe(false);
  });

  it("wears its exported token appearance and honors reduced motion", () => {
    const root = render(Dialog, {
      triggerLabel: "Open details",
      title: "Details",
      class: "max-w-xl",
    });
    const element = root.querySelector("dialog")!;

    expect(classes(element)).toEqual(classesOf(dialogAppearance({ class: "max-w-xl" })));
    expect(classes(element).has("motion-reduce:transition-none")).toBe(true);
  });
});
