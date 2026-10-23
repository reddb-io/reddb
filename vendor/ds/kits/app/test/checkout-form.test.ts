import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import CheckoutForm from "../src/composites/CheckoutForm.svelte";
import { checkoutForm } from "../src/composites/checkout-form.variants";
import CommerceContractFailures from "./fixtures/CommerceContractFailures.svelte";
import CommerceSurfacesConsumer from "./fixtures/CommerceSurfacesConsumer.svelte";
import { expectCommerceReadiness } from "./commerce-readiness";
import { classes, classesOf, render, rendered } from "./mount";

function checkoutErrorFailures(root: HTMLElement): string[] {
  const invalid = root.querySelector<HTMLElement>('[aria-invalid="true"]');
  if (!invalid) return ["checkout error has no invalid control"];
  const errorId = invalid?.getAttribute("aria-errormessage");
  const error = errorId ? root.querySelector<HTMLElement>(`#${errorId}`) : null;
  const described = new Set((invalid.getAttribute("aria-describedby") ?? "").split(/\s+/));
  return error && described.has(error.id)
    ? []
    : ["checkout error is not associated with its invalid control"];
}

describe("the deliberately failing Checkout form fixture", () => {
  it("diagnoses an error that is only visually adjacent to its field", () => {
    const broken = rendered(
      render(CommerceContractFailures, { failure: "checkout-error-is-unassociated" }),
    );

    expect(checkoutErrorFailures(broken)).toEqual([
      "checkout error is not associated with its invalid control",
    ]);
  });
});

describe("CheckoutForm", () => {
  const sections = [
    {
      legend: "Delivery",
      fields: [
        {
          name: "postalCode",
          label: "Postal code",
          value: "",
          required: true,
          help: "Use the delivery address.",
          error: "Enter a postal code.",
          autocomplete: "postal-code" as const,
        },
      ],
    },
  ] as const;

  it("composes native Form, Fieldset, Field, and Input associations", () => {
    const form = rendered(
      render(CheckoutForm, {
        sections,
        submitLabel: "Place order",
        error: "Review the highlighted field.",
        "aria-label": "Checkout",
      }),
    ) as HTMLFormElement;
    const input = form.querySelector<HTMLInputElement>('input[name="postalCode"]')!;
    const fieldError = form.querySelector<HTMLElement>("[data-field-invalid] [role='alert']")!;

    expect(form.tagName).toBe("FORM");
    expect(form.querySelector("fieldset > legend")?.textContent).toBe("Delivery");
    expect(form.querySelector(`label[for="${input.id}"]`)?.textContent).toContain("Postal code");
    expect(input.required).toBe(true);
    expect(input.getAttribute("aria-errormessage")).toBe(fieldError.id);
    expect(input.getAttribute("aria-describedby")?.split(" ")).toContain(fieldError.id);
    expect(checkoutErrorFailures(form)).toEqual([]);
    expect(form.getAttribute("aria-describedby")).toContain(
      form.querySelector<HTMLElement>("[data-checkout-error]")!.id,
    );
    expect(new FormData(form).get("postalCode")).toBe("");
  });

  it("keeps native keyboard focus order and a submit control", () => {
    const form = rendered(render(CheckoutForm, { sections, submitLabel: "Place order" }));
    const input = form.querySelector<HTMLInputElement>("input")!;
    const submit = form.querySelector<HTMLButtonElement>('button[type="submit"]')!;

    input.focus();
    expect(document.activeElement).toBe(input);
    submit.focus();
    expect(document.activeElement).toBe(submit);
  });

  it("adds a caller-owned composition seam without requiring convenience sections", () => {
    const content = createRawSnippet(() => ({
      render: () =>
        '<fieldset data-payment><legend>Payment</legend><label for="card">Card number</label><input id="card" name="card" /></fieldset>',
    }));
    const actions = createRawSnippet(() => ({
      render: () => '<button type="submit">Confirm order</button>',
    }));
    const form = rendered(
      render(CheckoutForm, {
        children: content,
        actions,
        "aria-label": "Advanced checkout",
      }),
    );

    expect(form.querySelector("[data-payment] legend")?.textContent).toBe("Payment");
    expect(form.querySelector('label[for="card"]')?.textContent).toBe("Card number");
    expect(form.querySelector('button[type="submit"]')?.textContent).toBe("Confirm order");
  });

  it("inherits nested appearance and keeps every gap Density-owned", () => {
    const scope = document.createElement("div");
    scope.dataset.theme = "application";
    scope.dataset.colorScheme = "dark";
    scope.dataset.density = "compact";
    const target = render(CheckoutForm, { sections, submitLabel: "Place order" });
    scope.append(...target.children);
    const form = scope.querySelector<HTMLElement>("[data-checkout-form]")!;
    const styles = checkoutForm();

    for (const name of classesOf(styles.root())) expect(classes(form)).toContain(name);
    // The Form owns its row rhythm from the layout tier, and a section's
    // fields keep the Fieldset's (ADR 0024): the composite re-spaces neither.
    expect(classes(form)).toContain("gap-[var(--reddb-spatial-layout-gap-md)]");
    for (const section of form.querySelectorAll("fieldset")) {
      expect(classes(section)).toContain("gap-[var(--reddb-spatial-layout-gap-sm)]");
    }
    expect(form.hasAttribute("data-theme")).toBe(false);
    expect(form.hasAttribute("data-color-scheme")).toBe(false);
    expect(form.hasAttribute("data-density")).toBe(false);
  });

  it("consumer-compiles, exports, showcases, and declares complete readiness", () => {
    expect(render(CommerceSurfacesConsumer).querySelector("[data-checkout-form]")).not.toBeNull();
    expectCommerceReadiness("checkout-form", "CheckoutForm");
  });
});
