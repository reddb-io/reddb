import { flushSync } from "svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { CodeBlock, codeBlock as codeBlockAppearance } from "@reddb-io/design-system/base";
import InlineTextContractFailures from "./fixtures/InlineTextContractFailures.svelte";
import { classes, classesOf, render, rendered } from "./mount";

const SOURCE = `function answer() {
  return 42;
}`;

afterEach(() => {
  Reflect.deleteProperty(navigator, "clipboard");
});

describe("the deliberately failing CodeBlock fixture", () => {
  it("demonstrates source whitespace lost outside preformatted semantics", () => {
    const code = rendered(
      render(InlineTextContractFailures, { failure: "collapsed-code", source: SOURCE }),
    );

    expect(code.tagName).toBe("CODE");
    expect(code.closest("pre")).toBeNull();
    expect(code.textContent).toBe("function answer() { return 42; }");
  });
});

describe("the Base CodeBlock", () => {
  it("preserves source whitespace with native pre and code semantics", () => {
    const root = rendered(render(CodeBlock, { code: SOURCE, language: "TypeScript" }));
    const pre = root.querySelector("pre")!;
    const code = pre.querySelector("code")!;

    expect(code.textContent).toBe(SOURCE);
    expect(root.querySelector("[data-code-block-language]")?.textContent).toBe("TypeScript");
    expect(classes(root)).toEqual(classesOf(codeBlockAppearance().root()));
  });

  it("copies the exact source from a keyboard-focusable canonical Button", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    const root = rendered(render(CodeBlock, { code: SOURCE }));
    const copy = root.querySelector<HTMLButtonElement>("[data-code-block-copy]")!;

    copy.focus();
    expect(document.activeElement).toBe(copy);
    copy.click();

    await vi.waitFor(() => expect(writeText).toHaveBeenCalledWith(SOURCE));
    await vi.waitFor(() => expect(copy.textContent?.trim()).toBe("Copied"));
  });
});

describe("the CodeBlock grammar (wave 5D)", () => {
  const keywords = new Set(["function", "return"]);
  const grammar = {
    tokenize(code: string) {
      return [...code.matchAll(/[A-Za-z_]\w*|\d+|\s+|[^\sA-Za-z_\d]+/g)].map(([value]) => ({
        kind: keywords.has(value) ? "keyword" : /^\d+$/.test(value) ? "number" : "text",
        value,
      }));
    },
  };

  it("renders synchronous token spans whose text is exactly the source", () => {
    const code = rendered(render(CodeBlock, { code: SOURCE, grammar })).querySelector("code")!;
    const spans = [...code.querySelectorAll("[data-code-token]")];
    expect(spans.length).toBeGreaterThan(3);
    expect(code.textContent).toBe(SOURCE);
    const keyword = spans.find((span) => span.textContent === "return")!;
    expect(keyword.getAttribute("data-code-token")).toBe("keyword");
    expect(classes(keyword).has("text-primary-text")).toBe(true);
    expect(spans.find((span) => span.textContent === "42")!.getAttribute("data-code-token")).toBe("number");
  });

  it("falls back to the exact source when a grammar would not reproduce it", () => {
    const lossy = { tokenize: () => [{ kind: "keyword", value: "function" }] };
    const code = rendered(render(CodeBlock, { code: SOURCE, grammar: lossy })).querySelector("code")!;
    expect(code.textContent).toBe(SOURCE);
  });

  it("renders plain text with no grammar", () => {
    const code = rendered(render(CodeBlock, { code: SOURCE })).querySelector("code")!;
    expect(code.querySelector("[data-code-token]")).toBeNull();
  });
});

describe("the CodeBlock scroll region (wave 5A)", () => {
  let measure: ResizeObserverCallback | undefined;
  beforeEach(() => {
    vi.stubGlobal(
      "ResizeObserver",
      class {
        constructor(callback: ResizeObserverCallback) {
          measure = callback;
        }
        observe(): void {}
        disconnect(): void {}
      },
    );
  });
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  /** jsdom lays nothing out: state the pre's scroll geometry, then let the observer fire. */
  function resize(pre: HTMLElement, scrollWidth: number): void {
    Object.defineProperty(pre, "scrollWidth", { configurable: true, value: scrollWidth });
    Object.defineProperty(pre, "clientWidth", { configurable: true, value: 300 });
    measure?.([], {} as ResizeObserver);
    flushSync();
  }

  it("adds no tab stop to a block that fits", () => {
    const pre = rendered(render(CodeBlock, { code: SOURCE, language: "TypeScript" })).querySelector("pre")!;
    resize(pre, 300);

    expect(pre.hasAttribute("tabindex")).toBe(false);
    expect(pre.hasAttribute("role")).toBe(false);
    expect(pre.hasAttribute("aria-label")).toBe(false);
  });

  it("becomes a named, keyboard-reachable region while it overflows, and stops when it fits", () => {
    const pre = rendered(render(CodeBlock, { code: SOURCE, language: "TypeScript" })).querySelector("pre")!;
    resize(pre, 900);
    expect(pre.getAttribute("tabindex")).toBe("0");
    expect(pre.getAttribute("role")).toBe("region");
    expect(pre.getAttribute("aria-label")).toBe("TypeScript code");
    pre.focus();
    expect(document.activeElement).toBe(pre);

    resize(pre, 300);
    expect(pre.hasAttribute("tabindex")).toBe(false);
    expect(pre.hasAttribute("role")).toBe(false);
  });

  it("names the region from regionLabel, or Code sample without a language", () => {
    const named = rendered(render(CodeBlock, { code: SOURCE, regionLabel: "Install command" })).querySelector("pre")!;
    resize(named, 900);
    expect(named.getAttribute("aria-label")).toBe("Install command");

    const plain = rendered(render(CodeBlock, { code: SOURCE })).querySelector("pre")!;
    resize(plain, 900);
    expect(plain.getAttribute("aria-label")).toBe("Code sample");
  });
});
