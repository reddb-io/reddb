import { createRawSnippet } from "svelte";
import { describe, expect, it, vi } from "vitest";
import {
  AlertDialog,
  alertDialog as alertDialogAppearance,
  alertDialogActions,
  button,
} from "./fixtures/drawer-alert-dialog-consumer";
import { classes, classesOf, render } from "./mount";

function text(content: string) {
  return createRawSnippet(() => ({ render: () => `<p>${content}</p>` }));
}

describe("the Base AlertDialog", () => {
  it("keeps destructive confirmation open when the outside surface is clicked", () => {
    const root = render(AlertDialog, {
      triggerLabel: "Delete deployment",
      title: "Delete deployment?",
      confirmLabel: "Delete",
      children: text("This cannot be undone."),
    });
    root.querySelector<HTMLButtonElement>("[data-dialog-trigger]")!.click();
    const dialog = root.querySelector<HTMLDialogElement>("dialog")!;

    expect(dialog.getAttribute("role")).toBe("alertdialog");
    dialog.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(dialog.open).toBe(true);
  });

  it("focuses the safe action and restores invoking focus after either decision", () => {
    const onconfirm = vi.fn();
    const root = render(AlertDialog, {
      triggerLabel: "Delete deployment",
      title: "Delete deployment?",
      confirmLabel: "Delete",
      cancelLabel: "Keep deployment",
      onconfirm,
      children: text("This cannot be undone."),
    });
    const trigger = root.querySelector<HTMLButtonElement>("[data-dialog-trigger]")!;
    trigger.focus();
    trigger.click();
    const dialog = root.querySelector<HTMLDialogElement>("dialog")!;
    const cancel = root.querySelector<HTMLButtonElement>("[data-alert-dialog-cancel]")!;

    expect(document.activeElement).toBe(cancel);
    cancel.click();
    expect(dialog.open).toBe(false);
    expect(document.activeElement).toBe(trigger);
    expect(onconfirm).not.toHaveBeenCalled();

    trigger.click();
    root.querySelector<HTMLButtonElement>("[data-alert-dialog-confirm]")!.click();
    expect(dialog.open).toBe(false);
    expect(document.activeElement).toBe(trigger);
    expect(onconfirm).toHaveBeenCalledTimes(1);
  });

  it("wears its exported token appearance and canonical Button actions", () => {
    const root = render(AlertDialog, {
      triggerLabel: "Delete deployment",
      title: "Delete deployment?",
      confirmLabel: "Delete",
      class: "consumer-alert-dialog",
    });
    const dialog = root.querySelector("dialog")!;
    const actions = root.querySelector("[data-alert-dialog-actions]")!;
    const cancel = root.querySelector("[data-alert-dialog-cancel]")!;
    const confirm = root.querySelector("[data-alert-dialog-confirm]")!;

    expect(classes(dialog)).toEqual(
      classesOf(alertDialogAppearance({ class: "consumer-alert-dialog" })),
    );
    expect(classes(actions)).toEqual(classesOf(alertDialogActions()));
    expect(classes(actions).has("gap-[var(--reddb-spatial-gap-md)]")).toBe(true);
    expect(classes(cancel)).toEqual(classesOf(button({ variant: "secondary", size: "sm" })));
    expect(classes(confirm)).toEqual(classesOf(button({ variant: "primary", tone: "danger", size: "sm" })));
    expect(classes(dialog).has("motion-reduce:transition-none")).toBe(true);
  });
});
