import { describe, expect, it } from "vitest";
import FieldInputConsumer from "./fixtures/FieldInputConsumer.svelte";
import FieldInputAccessibilityFailures from "./fixtures/FieldInputAccessibilityFailures.svelte";
import { Field, field } from "./fixtures/field-input-consumer";
import { classes, classesOf, render, rendered } from "./mount";

function control(root: HTMLElement): HTMLInputElement {
  const input = root.querySelector("input");
  expect(input, "the Field rendered no Input").not.toBeNull();
  return input!;
}

function associationFailures(root: HTMLElement): string[] {
  const failures: string[] = [];
  for (const input of root.querySelectorAll("input")) {
    const id = input.id;
    const labelled =
      input.hasAttribute("aria-label") ||
      (id !== "" && [...root.querySelectorAll("label")].some((label) => label.htmlFor === id));
    if (!labelled) failures.push("missing label association");

    const error = root.querySelector<HTMLElement>("[id$='-error']");
    if (input.getAttribute("aria-invalid") === "true" && error) {
      const describedBy = new Set((input.getAttribute("aria-describedby") ?? "").split(/\s+/));
      const errorMessage = input.getAttribute("aria-errormessage");
      if (!describedBy.has(error.id) || errorMessage !== error.id) {
        failures.push("missing error association");
      }
    }

    if (input.dataset.intendedRequired === "true" && !input.required) {
      failures.push("required was not forwarded");
    }
    if (input.dataset.intendedPattern && input.pattern !== input.dataset.intendedPattern) {
      failures.push("pattern was not forwarded");
    }
  }
  return failures;
}

describe("the Base Field", () => {
  it("is available with its Extension Seam through the Base consumer subpath", () => {
    expect(Field).toBeDefined();
    expect(field).toBeTypeOf("function");
  });

  it("owns the label and associates it with its control", () => {
    const root = rendered(render(FieldInputConsumer, {}));
    const input = control(root);
    const label = root.querySelector("label");

    expect(input.id).not.toBe("");
    expect(label?.getAttribute("for")).toBe(input.id);
    expect(label?.textContent?.trim()).toBe("Email address");
    expect(associationFailures(root)).toEqual([]);
  });

  it("uses an explicit control id when the consumer supplies one", () => {
    const root = rendered(render(FieldInputConsumer, { id: "account-email" }));
    expect(control(root).id).toBe("account-email");
    expect(root.querySelector("label")?.getAttribute("for")).toBe("account-email");
  });

  it("associates help and an announced error without replacing either", () => {
    const root = rendered(
      render(FieldInputConsumer, {
        id: "account-email",
        help: "We only use this for account notices.",
        error: "Enter a valid email address.",
      }),
    );
    const input = control(root);
    const describedBy = new Set((input.getAttribute("aria-describedby") ?? "").split(/\s+/));

    expect(describedBy).toEqual(new Set(["account-email-help", "account-email-error"]));
    expect(input.getAttribute("aria-errormessage")).toBe("account-email-error");
    expect(input.getAttribute("aria-invalid")).toBe("true");
    expect(root.querySelector("#account-email-help")?.textContent).toContain("account notices");
    expect(root.querySelector("#account-email-error")?.getAttribute("role")).toBe("alert");
    expect(associationFailures(root)).toEqual([]);
  });

  it("owns required semantics visibly and through the native control", () => {
    const root = rendered(render(FieldInputConsumer, { required: true }));
    expect(control(root).required).toBe(true);
    expect(root.querySelector("label")?.textContent).toContain("required");
  });

  it("wears exactly the classes exposed by its public variant seam", () => {
    const root = rendered(render(FieldInputConsumer, {}));
    const styles = field();
    expect(classes(root)).toEqual(classesOf(styles.root()));
    expect(classes(root.querySelector("label")!)).toEqual(classesOf(styles.label()));
  });
});

describe("the deliberately failing Field accessibility fixtures", () => {
  it.each([
    ["missing-label", ["missing label association"]],
    ["missing-error-association", ["missing error association"]],
    ["invalid-native-forwarding", ["missing label association", "required was not forwarded", "pattern was not forwarded"]],
  ] as const)("diagnoses %s", (failure, expected) => {
    const root = rendered(render(FieldInputAccessibilityFailures, { failure }));
    expect(associationFailures(root)).toEqual(expected);
  });
});
