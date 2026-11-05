import { flushSync } from "svelte";
import { describe, expect, it, vi } from "vitest";
import FormScaffoldingConsumer from "./fixtures/FormScaffoldingConsumer.svelte";
import { Form, form } from "./fixtures/form-scaffolding-consumer";
import { classes, classesOf, render, rendered } from "./mount";

describe("the Base Form", () => {
  it("is available with its Extension Seam through the Base consumer subpath", () => {
    expect(Form).toBeDefined();
    expect(form).toBeTypeOf("function");
  });

  it("is a native form and forwards its submission contract", () => {
    const onsubmit = vi.fn((event: SubmitEvent) => event.preventDefault());
    const root = rendered(render(FormScaffoldingConsumer, { onsubmit }));
    const element = root.querySelector<HTMLFormElement>('[data-testid="profile-form"]')!;

    expect(element.tagName).toBe("FORM");
    expect(element.method).toBe("post");
    expect(element.getAttribute("action")).toBe("/profiles");
    expect(element.getAttribute("autocomplete")).toBe("off");
    expect(element.noValidate).toBe(true);

    const submitted = element.dispatchEvent(
      new SubmitEvent("submit", { bubbles: true, cancelable: true }),
    );
    flushSync();
    expect(submitted).toBe(false);
    expect(onsubmit).toHaveBeenCalledTimes(1);
  });

  it("composes canonical labels, groups, and controls into native form data", () => {
    const root = rendered(render(FormScaffoldingConsumer, {}));
    const element = root.querySelector<HTMLFormElement>('[data-testid="profile-form"]')!;
    const name = element.querySelector<HTMLInputElement>('#display-name')!;
    const notices = element.querySelector<HTMLInputElement>('#email-notices')!;

    name.value = "Ada";
    notices.checked = true;

    expect(element.querySelector('label[for="display-name"]')).not.toBeNull();
    expect(element.querySelector("fieldset > legend")?.textContent).toContain(
      "Contact preferences",
    );
    expect(Object.fromEntries(new FormData(element))).toEqual({
      displayName: "Ada",
      notices: "email",
    });
  });

  it("wears exactly its exported appearance and merges consumer classes", () => {
    const root = rendered(render(FormScaffoldingConsumer, {}));
    const element = root.querySelector<HTMLFormElement>('[data-testid="profile-form"]')!;

    expect(classes(element)).toEqual(classesOf(form({ class: "max-w-xl" })));
    // The Form owns its row rhythm from the Density layout tier (ADR 0024).
    expect(classes(element).has("gap-[var(--reddb-spatial-layout-gap-md)]")).toBe(true);
    expect(classes(element).has("max-w-xl")).toBe(true);
  });

  it("inherits a nested appearance without selecting any axis itself", () => {
    const root = rendered(render(FormScaffoldingConsumer, {}));
    const element = root.querySelector<HTMLFormElement>(
      '[data-appearance-scope] [data-testid="nested-form"]',
    )!;

    expect(element.hasAttribute("data-theme")).toBe(false);
    expect(element.hasAttribute("data-color-scheme")).toBe(false);
    expect(element.hasAttribute("data-density")).toBe(false);
    expect(classes(element)).toEqual(classesOf(form()));
  });
});
