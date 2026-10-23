import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import CodeField from "../src/CodeField.svelte";
import { variableTokenGrammar } from "../src/code-field";

const mounted: Array<{ instance: Record<string, unknown>; target: HTMLElement }> = [];

function render(props: Record<string, unknown>): HTMLElement {
  const target = document.createElement("div");
  document.body.append(target);
  const instance = mount(CodeField, { target, props }) as Record<string, unknown>;
  mounted.push({ instance, target });
  flushSync();
  return target;
}

function press(element: Element, key: string): KeyboardEvent {
  const event = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true });
  element.dispatchEvent(event);
  flushSync();
  return event;
}

afterEach(() => {
  for (const entry of mounted.splice(0)) {
    unmount(entry.instance);
    entry.target.remove();
  }
});

describe("the reference variable token grammar", () => {
  it("tokenizes resolved and unresolved {{variable}} references without losing source text", () => {
    const grammar = variableTokenGrammar(["customer.name", "order_id"]);
    const source = "Hello {{ customer.name }}: {{missing}} / {{order_id}}";
    const tokens = grammar.tokenize(source);

    expect(tokens).toEqual([
      { kind: "text", value: "Hello ", start: 0, end: 6 },
      {
        kind: "variable",
        value: "{{ customer.name }}",
        name: "customer.name",
        state: "resolved",
        start: 6,
        end: 25,
      },
      { kind: "text", value: ": ", start: 25, end: 27 },
      {
        kind: "variable",
        value: "{{missing}}",
        name: "missing",
        state: "unresolved",
        start: 27,
        end: 38,
      },
      { kind: "text", value: " / ", start: 38, end: 41 },
      {
        kind: "variable",
        value: "{{order_id}}",
        name: "order_id",
        state: "resolved",
        start: 41,
        end: 53,
      },
    ]);
    expect(tokens.map((token) => token.value).join("")).toBe(source);
  });

  it("leaves incomplete delimiters as text and exposes their completion query", () => {
    const grammar = variableTokenGrammar();

    expect(grammar.tokenize("Before {{cus")).toEqual([
      { kind: "text", value: "Before {{cus", start: 0, end: 12 },
    ]);
    expect(grammar.completionAt?.("Before {{cus", 12)).toEqual({
      query: "cus",
      replaceStart: 9,
      replaceEnd: 12,
    });
    expect(
      grammar.complete?.("Before {{cus", { query: "cus", replaceStart: 9, replaceEnd: 12 }, "customer"),
    ).toEqual({ value: "Before {{customer}}", cursor: 19 });
  });
});

describe("the Data Kit CodeField", () => {
  it("keeps the highlight overlay synchronized with input content and scrolling", () => {
    const onvaluechange = vi.fn();
    const grammar = variableTokenGrammar(["known"]);
    const root = render({ label: "Template", value: "Hello {{known}}", grammar, onvaluechange });
    const field = root.querySelector<HTMLElement>("[data-code-field]")!;
    const input = field.querySelector<HTMLInputElement>("[data-code-input]")!;
    const highlight = field.querySelector<HTMLElement>("[data-code-highlight]")!;

    expect(root.querySelector(`label[for="${input.id}"]`)?.textContent).toContain("Template");
    expect(highlight.textContent).toBe(input.value);
    expect(highlight.querySelector('[data-token-state="resolved"]')?.textContent).toBe("{{known}}");

    input.value = "Hello {{unknown}} and {{known}}";
    input.setSelectionRange(input.value.length, input.value.length);
    input.dispatchEvent(new InputEvent("input", { bubbles: true }));
    flushSync();

    expect(highlight.textContent).toBe(input.value);
    expect(highlight.querySelector('[data-token-state="unresolved"]')?.textContent).toBe(
      "{{unknown}}",
    );
    expect(onvaluechange).toHaveBeenLastCalledWith(input.value);

    input.scrollLeft = 28;
    input.dispatchEvent(new Event("scroll"));
    expect(highlight.scrollLeft).toBe(28);
  });

  it("operates autocomplete from the keyboard while focus remains on the input", () => {
    const onvaluechange = vi.fn();
    const root = render({
      label: "Template",
      value: "Hello {{us",
      suggestions: [
        { value: "user.name", label: "User name" },
        { value: "user.id", label: "User id" },
        { value: "account.id", label: "Account id" },
      ],
      onvaluechange,
    });
    const input = root.querySelector<HTMLInputElement>("[data-code-input]")!;
    input.focus();
    input.setSelectionRange(input.value.length, input.value.length);
    input.dispatchEvent(new InputEvent("input", { bubbles: true }));
    flushSync();

    expect(input.getAttribute("role")).toBe("combobox");
    expect(input.getAttribute("aria-autocomplete")).toBe("list");
    expect(input.getAttribute("aria-expanded")).toBe("true");
    expect(
      [...root.querySelectorAll('[role="option"]')].map((option) => option.textContent?.trim()),
    ).toEqual(["User name", "User id"]);

    expect(press(input, "ArrowDown").defaultPrevented).toBe(true);
    const activeId = input.getAttribute("aria-activedescendant");
    expect(activeId).not.toBeNull();
    expect(document.getElementById(activeId!)?.getAttribute("role")).toBe("option");
    expect(document.activeElement).toBe(input);

    expect(press(input, "Enter").defaultPrevented).toBe(true);
    expect(input.value).toBe("Hello {{user.name}}");
    expect(onvaluechange).toHaveBeenLastCalledWith("Hello {{user.name}}");
    expect(input.getAttribute("aria-expanded")).toBe("false");
    expect(document.activeElement).toBe(input);
  });

  it("keeps the multi-line editor a native textbox, never role=combobox (ARIA in HTML)", () => {
    const root = render({
      label: "Script",
      value: "{{us",
      gutter: true,
      suggestions: [{ value: "user.name", label: "User name" }],
    });
    const editor = root.querySelector<HTMLTextAreaElement>("textarea[data-code-input]")!;
    editor.focus();
    editor.setSelectionRange(editor.value.length, editor.value.length);
    editor.dispatchEvent(new InputEvent("input", { bubbles: true }));
    flushSync();

    expect(editor.hasAttribute("role")).toBe(false);
    expect(editor.hasAttribute("aria-expanded")).toBe(false);
    expect(editor.getAttribute("aria-autocomplete")).toBe("list");
    const listbox = document.getElementById(editor.getAttribute("aria-controls")!);
    expect(listbox?.getAttribute("role")).toBe("listbox");

    press(editor, "ArrowDown");
    expect(document.getElementById(editor.getAttribute("aria-activedescendant")!)?.getAttribute("role")).toBe("option");
    expect(document.activeElement).toBe(editor);
  });

  it("centres a single line by its line box and draws a control edge at every Density stop", () => {
    const root = render({ label: "Request URL", value: "{{api_url}}/health", resolved: ["api_url"] });
    const field = root.querySelector<HTMLElement>("[data-code-field]")!;
    const input = field.querySelector<HTMLElement>("[data-code-input]")!;
    const highlight = field.querySelector<HTMLElement>("[data-code-highlight]")!;
    const box = highlight.parentElement!;
    const line = "leading-[calc(var(--reddb-spatial-control-height-md)-2px)]";

    for (const layer of [input, highlight]) {
      expect(layer.classList.contains("py-0"), layer.tagName).toBe(true);
      expect(layer.classList.contains(line), layer.tagName).toBe(true);
    }
    expect(box.classList.contains("h-[var(--reddb-spatial-control-height-md)]")).toBe(true);
    expect(input.classList.contains("border-control-edge")).toBe(true);
    expect(box.classList.contains("border-muted")).toBe(false);
  });

  it("renders cleanly across Theme × Color Scheme × Density in single-line and gutter modes", () => {
    for (const theme of ["base", "editorial"]) {
      for (const scheme of ["light", "dark"]) {
        for (const density of ["tiny", "compact", "comfortable", "spacious"]) {
          for (const gutter of [false, true]) {
            const root = render({
              label: gutter ? "Query" : "Expression",
              value: gutter ? "select {{column}}\nfrom {{table}}" : "{{column}} + 1",
              gutter,
              resolved: ["column"],
            });
            root.dataset.theme = theme;
            root.dataset.colorScheme = scheme;
            root.dataset.density = density;

            const field = root.querySelector<HTMLElement>("[data-code-field]")!;
            const input = field.querySelector<HTMLInputElement | HTMLTextAreaElement>(
              "[data-code-input]",
            )!;
            expect(field.dataset.mode).toBe(gutter ? "gutter" : "single-line");
            expect(input.tagName).toBe(gutter ? "TEXTAREA" : "INPUT");
            expect(field.textContent).not.toMatch(/undefined|NaN|\[object Object\]/);
            expect(field.hasAttribute("data-theme")).toBe(false);
            expect(field.hasAttribute("data-color-scheme")).toBe(false);
            expect(field.hasAttribute("data-density")).toBe(false);
            expect(field.querySelectorAll("[data-code-line-number]")).toHaveLength(gutter ? 2 : 0);
          }
        }
      }
    }
  });
});
